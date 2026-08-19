//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO o contra el DAEMON (fase 3 M2, por `[daemon] mode` o
//! `--daemon`), con keymap engine (ADR 0006). Regla 7: solo cambia el
//! transporte.
//! `norte_tui::tty::init/restore` gestionan raw mode + pantalla alternativa
//! con hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
//! Pintan sobre la terminal DE CONTROL (`tty.rs`), no sobre stdout: desde
//! `--pick` stdout lleva datos, no secuencias de escape.
#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::VPath;
use norte_tui::app::App;
use norte_tui::config::{self, WatchMode};
use norte_tui::event_loop::run;
use norte_tui::help::TuiChords;
use norte_tui::hints::DialogHints;
use norte_tui::keymap::Resolver;
use norte_tui::listing::initial_pane;
use norte_tui::mouse;
use norte_tui::navigate::cache_capabilities;
use norte_tui::screens::apply_theme;
use norte_tui::session_push::restore_session;
use norte_tui::shortcuts_editor::build_keymaps;
use norte_tui::tty;
use norte_vfs_local::LocalProvider;
use std::sync::Arc;

#[tokio::main]
#[allow(clippy::too_many_lines)] // wiring del binario, no API — mismo criterio que `run`/`dispatch`
async fn main() -> Result<()> {
    // Args: DIR posicional + `--preset`/`--daemon`/`--socket`. `--help` y
    // `--version` salen ANTES de tocar el terminal (antes se ignoraban como
    // flag desconocido y el binario moría al no poder abrir la TTY).
    let parsed = norte_frontend::cli::parse(std::env::args_os().skip(1), BOOL_FLAGS, VALUE_FLAGS);
    let Some(args) = args_or_exit(parsed)? else {
        return Ok(()); // `--help`/`--version`: ya impreso.
    };
    let (cli_preset, cli_layout, cli_daemon, cli_socket, cli_pick, cli_cd_file) = (
        args.text("--preset"),
        // `--layout` NO es texto por contrato: acaba siendo un nombre de
        // fichero, y por `to_string_lossy` dos bytes inválidos distintos
        // abrían el mismo `\u{FFFD}.toml` (#246 M1).
        args.os_text("--layout").map(std::ffi::OsString::from),
        args.has("--daemon"),
        args.path("--socket"),
        args.has("--pick"),
        args.path("--cd-file"),
    );
    let layers = config::standard_layers();
    let cfg = config::load_async(layers.clone())
        .await
        .context("config inválida")?;
    // Idioma: NORTE_LANG explícito > [ui] lang de la config > entorno.
    let lang = if std::env::var("NORTE_LANG").is_ok_and(|v| !v.is_empty()) {
        norte_i18n::Lang::from_env()
    } else if let Some(l) = &cfg.common.ui_lang {
        norte_i18n::Lang::negotiate(Some(l))
    } else {
        norte_i18n::Lang::from_env()
    };
    let _ = norte_i18n::force(lang);
    // Roadmap ítem 9: el log va al FICHERO y solo al fichero. Hasta aquí este
    // binario no instalaba subscriber ninguno y lo decía en un comentario más
    // abajo: un `fmt` a stderr pelea con la pantalla alternativa, así que cada
    // `tracing::warn!` de la TUI se descartaba mudo.
    //
    // Va DESPUÉS de cargar la config porque `[log] dir` sale de ella, lo que
    // significa que un `--help`/`--version` —que salen antes— no deja rastro.
    // Correcto: no hacen nada que merezca un log.
    //
    norte_core::logging::init_to_file(norte_core::logging::LogConfig {
        dir: cfg.common.log_dir.as_deref(),
        retain: cfg.common.log_retain,
    });
    let (browse_eff, viewer_eff, dialog_eff) = build_keymaps(&cfg, cli_preset.as_deref())?;
    // Bindings `lua:` descartados del keymap.toml de PROYECTO (seguridad,
    // review M4 Lua): se avisa tras crear la App, jamás descarte mudo. El
    // contexto `global` se fusiona en las TRES pantallas (H1 T2 suma
    // dialog), así que el máximo es el recuento sin dobles (un binding
    // global cuenta en todas).
    let discarded_lua = browse_eff
        .discarded_lua_bindings()
        .max(viewer_eff.discarded_lua_bindings())
        .max(dialog_eff.discarded_lua_bindings());

    let mut backend = make_backend(&cfg, cli_daemon, cli_socket).await?;

    // El DIR posicional manda sobre el `cwd`; se valida aquí para dar un
    // error claro en vez de un listado fallido dentro del TUI ya arrancado.
    let start = start_dir(args.dir)?;
    // #108 b4: columnas y orden desde `[ui.columns]` — resuelto UNA vez;
    // los ids inválidos no rompen el arranque (doctor los reporta). ANTES de
    // los listados iniciales (#117): ellos también piden los attrs
    // configurados — sin esto, las celdas attr nacen en blanco hasta el
    // primer cd/refresh.
    let columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
    let start_attrs = columns.attr_ids_for(start.scheme());
    let left = initial_pane(&backend, &start, &start_attrs).await?;
    let right = initial_pane(&backend, &start, &start_attrs).await?;
    let mut app = App::new(left, right);
    app.pick = cli_pick; // `--pick` (S2): see the field's rustdoc (`app.rs`).
    app.columns = columns;
    // Sincronizar necesita journal Y spool (regla dura 4: `sync.apply` abre un
    // lote deshacible y se niega sin él; `sync.plan` se niega sin spool). Desde
    // #167 el brazo embebido SÍ lleva el journal del directorio de estado (que
    // desde #177 abre en su primera mutación), pero sigue sin spool, así que
    // `is_journalled()` sigue diciendo que no — y dice la verdad sobre lo único
    // que atenúa, que es sincronizar. Se decide UNA vez, aquí, porque el
    // `Backend` no cambia de brazo en vida del proceso.
    app.backend_journalled = backend.is_journalled();
    // #117: el catálogo del scheme de arranque — incondicional, como el cd
    // (una vez por scheme y sesión; el picker de la tarea 4 lo quiere
    // aunque no haya columnas attr configuradas); un fallo NO tumba el
    // arranque — sin catálogo se pinta con defaults Opaque.
    // H3d: la MISMA respuesta trae las caps (`fs.capabilities` devuelve las
    // dos mitades), así que se cachean juntas — sin ellas la ayuda del primer
    // F1 caería al criterio sintáctico teniendo el dato al alcance.
    if let Ok(both) = backend.capabilities_and_attrs(&start).await {
        cache_capabilities(&mut app, &start, both);
    }
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    // #107: `[ui] show_hidden` siembra el estado INICIAL de ambos panes;
    // Ctrl+H lo cambia por pane en runtime (el hot-reload no lo pisa — un
    // toggle del usuario no debe deshacerse porque otro campo cambió).
    if let Some(show) = cfg.common.ui_show_hidden {
        for pane in &mut app.panes {
            pane.set_show_hidden(show);
        }
    }
    // `[ui] layout`: una disposición guardada. Un layout que no carga NO deja
    // a norte sin pantalla — se avisa por la barra y se arranca con
    // `orthodox`, que es lo que el usuario tenía antes de escribir la clave.
    // `--layout` gana a `[ui] layout`: elegir una disposición para UN arranque
    // no debe tocar tu config, que es justo lo que hace la clave.
    let nombre_layout: Option<std::ffi::OsString> = cli_layout
        .clone()
        .or_else(|| cfg.common.ui_layout.clone().map(std::ffi::OsString::from))
        .filter(|n| n != std::ffi::OsStr::new("orthodox"));
    if let Some(nombre) = nombre_layout {
        // El fichero se lee FUERA del runtime (regla 2), y sin directorio de
        // config no hay fichero que valga: queda el preset de ese nombre.
        let cargado = match config::user_config_dir() {
            Some(dir) => {
                let n = nombre.clone();
                tokio::task::spawn_blocking(move || norte_frontend::layout::config::load(&dir, &n))
                    .await
                    .unwrap_or_else(|_| {
                        Err(norte_frontend::layout::LayoutError::NotFound(String::new()))
                    })
            }
            None => Err(norte_frontend::layout::LayoutError::NotFound(String::new())),
        };
        app.apply_loaded_layout(&nombre, cargado);
    }
    // L2: la pantalla que dejaste. Va DESPUÉS de `[ui] layout` a propósito —
    // una sesión guardada es más específica que una preferencia de config, y
    // es la que gana— y antes del tema, que no depende de ninguna de las dos.
    // Un fallo NO tumba el arranque: se sigue con la pantalla de la config.
    restore_session(&mut app, &backend).await;
    apply_theme(&mut app, &cfg);
    // Copia de la hotlist en el App (spec 2026-07-18): la fuente del popup
    // `Ctrl+D`; se refresca en cada hot-reload OK (`reload_config`).
    app.hotlist = cfg.common.hotlist.clone();
    // Hints de pie de página de los overlays (H1 T3, #24): PRECOMPUTADOS del
    // efectivo `dialog` ANTES de que se mueva al `Resolver` de abajo — igual
    // que `help_lines`, se reconstruyen en cada hot-reload OK.
    app.dialog_hints = DialogHints::build(&dialog_eff);
    // Openers declarativos (#28): fuente de `pane.open` (F4).
    app.openers = cfg.openers.clone();
    // Canales del modo daemon (None en embebido): tasks de otros frontends
    // y avisos de (re)conexión — se drenan en el loop principal.
    let foreign_tasks = backend.take_foreign_tasks();
    let conn_events = backend.take_conn_events();
    let approvals = backend.take_approvals();
    // #44: avisos `connection.degraded` del daemon → indicador persistente.
    let degraded = backend.take_degraded();
    // #167/#177: el brazo embebido abre el journal en su primera mutación, y si
    // resulta que lo tiene otro proceso, esta sesión muta SIN registro. Eso se
    // dice EN la sesión y en el instante en que ocurre: un `eprintln!` de
    // arranque lo taparía la pantalla alternativa un segundo después, y aquí ni
    // siquiera se sabe al arrancar. (Un indicador permanente en la barra sería
    // mejor que un mensaje que el siguiente borra; sigue pendiente.)
    let journal_warnings = backend.take_journal_warnings();
    let mut help_lines = norte_tui::help::build(&browse_eff, &viewer_eff, &dialog_eff);
    // H3b: the chord resolver the help corpus is rendered through. Built from
    // the SAME three effectives as `help_lines` and BEFORE they move into the
    // `Resolver`s below (it borrows), and rebuilt alongside them on every hot
    // reload — the obligation `TuiChords`' own rustdoc states: a rebind that
    // does not reach this resolver is a page that teaches the OLD key.
    app.help_chords = Arc::new(TuiChords::new(&browse_eff, &viewer_eff, &dialog_eff, lang));
    // Filas de la command palette (H1 T4): PRECOMPUTADAS de los efectivos
    // browse/viewer ANTES de que se muevan al `Resolver` de abajo — mismo
    // criterio que `help_lines`/`dialog_hints`.
    app.palette_rows = norte_tui::palette::build_rows(&browse_eff, &viewer_eff);
    let mut resolver = Resolver::new(browse_eff);
    let mut viewer_resolver = Resolver::new(viewer_eff);
    // H1 T2: resolver compartido por TODOS los overlays (modal, theme
    // picker, extensions, nav popup) — mutuamente exclusivos en el run loop
    // (el `if`/`else if` de más abajo), así que un único estado de secuencia
    // basta. Los presets `[dialog]` son de UN chord; un `Resolution::Pending`
    // (solo posible con una secuencia multi-tecla de una capa de usuario) se
    // trata como ignorar-y-reiniciar en cada handler — sin semántica de
    // overlay definida para eso todavía.
    let mut dialog_resolver = Resolver::new(dialog_eff);

    // Hot-reload: vigilancia de las capas, con aviso si degrada a polling.
    let (cfg_tx, cfg_rx) = tokio::sync::mpsc::channel(8);
    let watch = config::watch(&layers, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some(t("msg-config-polling"));
    }
    // DESPUÉS del aviso de polling: el de seguridad no debe quedar pisado.
    if discarded_lua > 0 {
        app.message = Some(ta(
            "msg-lua-keymap-project",
            &[("n", &discarded_lua.to_string())],
        ));
    }

    let (tty_out, mut mouse_out) = open_terminal_or_exit()?;
    let mut terminal = tty::init(tty_out)?;
    let mut capture = arm_mouse(&cfg, &mut app, &mut mouse_out);
    let res = run(
        &mut terminal,
        &mut capture,
        &mut app,
        &backend,
        &mut resolver,
        &mut viewer_resolver,
        &mut dialog_resolver,
        &mut help_lines,
        lang,
        layers,
        cli_preset,
        cfg.quick_search_mode,
        cfg.common.ui_confirm_quit,
        cfg,
        cfg_rx,
        foreign_tasks,
        conn_events,
        approvals,
        degraded,
        journal_warnings,
    )
    .await;
    let _ = capture.set(false, terminal.backend_mut());
    restore_terminal(&mut terminal);
    drop(watch);
    res?; // A broken run loop is not a cancelled `--pick`.
    write_cd_file(&app, cli_cd_file.as_deref());
    finish_pick(&mut app);
    Ok(())
}

/// `--cd-file` (S3): writes the final directory for the `norte shell-init`
/// wrapper to read, on a clean quit only. A no-op when the flag was never
/// passed.
///
/// Placed after `res?`, not before: a run loop that returned an error bails
/// out of `main` right there and never reaches this call, so a crash writes
/// nothing to the cd-file — the design's own rule (§C: "the write happens at
/// the end", so the shell stays where it was).
///
/// Placed after [`restore_terminal`] too, deliberately DIFFERENT from the
/// design note's "before restoring the terminal": [`finish_pick`]
/// establishes, for the exact same shutdown window, that nothing must be
/// printed before the alternate screen is left or the terminal swallows it.
/// The `msg-cd-not-local` line below is exactly such a print, so it follows
/// `finish_pick`'s placement, not the design prose. Still runs BEFORE
/// `finish_pick` itself, whose `std::process::exit` would otherwise skip
/// this entirely when both `--pick` and `--cd-file` are given.
///
/// A write failure is printed and swallowed, the same shape as
/// `restore_terminal`'s own failure: `--cd-file`'s exit codes are not
/// contracted the way `--pick`'s are (design §B's 0/1/2 table is that flag's
/// alone), and nothing downstream is waiting on this process's exit code the
/// way a shell wrapper waits on `--pick`'s.
fn write_cd_file(app: &App, cd_file: Option<&std::path::Path>) {
    let Some(path) = cd_file else { return };
    if let Some(bytes) = norte_frontend::shell::cd_bytes(app.focused().dir()) {
        use std::io::Write as _;
        let wrote = std::fs::File::create(path)
            .and_then(|mut f| f.write_all(&bytes).and_then(|()| f.flush()));
        if let Err(e) = wrote {
            eprintln!("ntc: failed to write --cd-file: {e}");
        }
    } else {
        // Same masking convention as the CLI's own plain-text output
        // (`norte-cli/src/main.rs`'s semantic-search listing): `path_display`
        // gives the badge as a bool because a raw stderr line has no
        // styling to hang it on, so a hostile name is marked with a
        // leading `!` instead of colour.
        let (texto, hostil) = norte_frontend::path_display(app.focused().dir());
        let marcado = if hostil { format!("!{texto}") } else { texto };
        eprintln!("{}", ta("msg-cd-not-local", &[("path", &marcado)]));
    }
}

/// `--pick` (S2): the picker's exit, decided AFTER the terminal is restored
/// — never before, or the alternate screen swallows every byte (the whole
/// point of Task 1). Exit codes per the design's table: 0 accepted
/// (written), 1 cancelled (nothing written — `q`/`F10` under `--pick` never
/// populate `app.picked`), 2 reserved for the no-tty error in
/// [`open_terminal_or_exit`] and, here, a write failure the caller needs to
/// tell apart from "user picked nothing".
///
/// Returns normally only when `--pick` was never passed: every other path
/// exits the process directly, so `main` never reaches its own `Ok(())`
/// with a pick outstanding.
fn finish_pick(app: &mut App) {
    if let Some(paths) = app.picked.take() {
        use std::io::Write as _;
        let bytes = norte_frontend::shell::pick_bytes(&paths);
        let mut stdout = std::io::stdout();
        if let Err(e) = stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
            eprintln!("ntc: failed to write the pick: {e}");
            std::process::exit(2);
        }
        std::process::exit(0);
    }
    if app.pick {
        std::process::exit(1);
    }
}

/// Deshace [`tty::init`]. Lo mismo que hacía `ratatui::restore()`: no hay
/// mucho que hacer si falla, así que se imprime y se sigue saliendo.
fn restore_terminal(terminal: &mut tty::Tui) {
    if let Err(e) = tty::restore(terminal) {
        eprintln!("ntc: failed to restore terminal: {e}");
    }
}

/// Abre la terminal DE CONTROL (`tty.rs`, no stdout — desde `--pick` stdout
/// lleva datos) y un segundo descriptor duplicado para `arm_mouse`, que se
/// llama antes de que `run` reciba la `Tui` y por tanto no puede tomar
/// prestado el handle que se mueve a [`tty::init`] (dueño único del
/// backend).
///
/// Sin controladora (cron, ambos extremos con pipe) es un error legible y
/// código de salida 2 — jamás un pantallazo de escapes en el pipe de quien
/// nos invocó.
fn open_terminal_or_exit() -> Result<(tty::TtyOut, tty::TtyOut)> {
    let out = match tty::open_controlling_terminal() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("ntc: no controlling terminal: {e}");
            std::process::exit(2);
        }
    };
    let mouse_out = out
        .try_clone()
        .context("no se pudo duplicar el descriptor de la terminal")?;
    Ok((out, mouse_out))
}

/// Pide la captura de ratón si `[ui] mouse` no la desactiva (default ON).
///
/// Se pide ANTES de entrar al run loop y `main` la retira SIEMPRE al salir,
/// pase lo que pase: una terminal devuelta en modo ratón escupe secuencias
/// de escape en cuanto el usuario mueve el puntero, y para entonces ya no
/// queda nadie escuchándolas.
///
/// Un emulador que no acepte la secuencia no es motivo para no arrancar: se
/// sigue sin ratón y se dice por la barra, jamás en silencio (el usuario
/// hará click y no pasará nada).
fn arm_mouse(cfg: &config::LoadedConfig, app: &mut App, out: &mut tty::TtyOut) -> mouse::Capture {
    // El hook de pánico de `tty::init` ya suelta pantalla alternativa, raw
    // mode Y ratón sobre un handle propio — pero la captura de ratón es un
    // DECSET de la terminal ENTERA, no algo que se vaya con la pantalla, así
    // que se envuelve otra vez aquí por si esta función algún día arma algo
    // que `tty::init` no sepa deshacer. Mismo patrón: se toma el hook
    // vigente y se sustituye por uno que primero suelta el ratón y LUEGO lo
    // llama. El closure no puede tomar prestado `out` (el préstamo no
    // sobrevive a esta función), así que abre un handle nuevo a la terminal
    // de control en el momento del pánico — igual que hace `tty::init`.
    let previo = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut tty_out) = tty::open_controlling_terminal() {
            let _ = crossterm::execute!(tty_out, crossterm::event::DisableMouseCapture);
        }
        previo(info);
    }));
    let mut capture = mouse::Capture::new();
    if let Err(e) = capture.set(cfg.common.ui_mouse.unwrap_or(true), out) {
        tracing::warn!(error = %e, "no se pudo activar la captura de ratón");
        app.message = Some(t("msg-mouse-capture-failed"));
    }
    capture
}

/// Flags booleanos del TUI.
const BOOL_FLAGS: &[&str] = &["--daemon", "--pick"];
/// Flags con valor del TUI.
const VALUE_FLAGS: &[&str] = &["--preset", "--layout", "--socket", "--cd-file"];

/// Texto de `--help`. En INGLÉS y sin Fluent a propósito: se imprime ANTES
/// de negociar el idioma (que sale de la config, que aún no se ha leído).
const USAGE: &str = "\
ntc — orthodox file manager, terminal frontend

Usage: ntc [OPTIONS] [DIR]

Arguments:
  [DIR]  Directory to start in (default: the current directory)

Options:
      --preset <NAME>    Keymap preset (orthodox|vim|cua); overrides norte.toml
      --layout <NAME>    Layout for this run (orthodox|simple|krusader|explorer|full,
                         or one of your own under `layouts/`); overrides norte.toml
      --daemon           Talk to the daemon instead of the embedded core
      --socket <PATH>    Daemon socket (default: $XDG_RUNTIME_DIR/norte/daemon.sock)
      --pick             print the selection, NUL-terminated, and exit
      --cd-file PATH     write the final directory here, NUL-terminated
                         (used by the `norte shell-init` wrapper)
  -h, --help             Print help
  -V, --version          Print version
";

/// Directorio de arranque como [`VPath`]: el `[DIR]` de la línea de
/// comandos si vino, si no el `cwd`. Se valida ANTES de tomar el terminal
/// para dar un error legible en vez de un listado fallido dentro del TUI ya
/// arrancado.
///
/// Un cwd UNC de Windows (`\\server\share`, `\\wsl$\…`) ya round-trip-ea:
/// `vpath_from_native` lo mete como primer segmento y `to_native` lo
/// restituye como base de la raíz del OS (#22). Uno irrepresentable da
/// error claro, jamás un panic.
fn start_dir(dir: Option<std::path::PathBuf>) -> Result<VPath> {
    let nativo = match dir {
        Some(d) => {
            let meta = std::fs::metadata(&d)
                .with_context(|| format!("no se puede abrir {}", d.display()))?;
            anyhow::ensure!(meta.is_dir(), "{} no es un directorio", d.display());
            std::path::absolute(&d).unwrap_or(d)
        }
        None => std::env::current_dir().context("cwd")?,
    };
    norte_vfs_local::vpath_from_native(&nativo)
        .map_err(|e| anyhow::anyhow!("{} no representable como VPath: {e}", nativo.display()))
}

/// Resuelve los argumentos "de salida inmediata": imprime `--help`/
/// `--version` (devolviendo `None`, el caller termina) y convierte un flag
/// desconocido en error. Antes `--help` caía en el brazo de "ignora" y el
/// binario seguía hasta intentar tomar la TTY, donde moría con un panic de
/// ratatui.
fn args_or_exit(args: norte_frontend::cli::Cli) -> Result<Option<norte_frontend::cli::Cli>> {
    if args.help {
        print!("{USAGE}");
        return Ok(None);
    }
    if args.version {
        println!("ntc {}", env!("CARGO_PKG_VERSION"));
        return Ok(None);
    }
    if let Some(flag) = &args.unknown {
        anyhow::bail!("unknown flag `{flag}` — try `ntc --help`");
    }
    Ok(Some(args))
}

/// Elige el transporte (regla 7): `--daemon` o `[daemon] mode = daemon`
/// conecta al socket (arrancando `norte daemon run` si hace falta);
/// cualquier otra cosa = embebido (arranque instantáneo, el default).
async fn make_backend(
    cfg: &config::LoadedConfig,
    cli_daemon: bool,
    cli_socket: Option<std::path::PathBuf>,
) -> Result<Backend> {
    let want_daemon = cli_daemon || cfg.common.daemon_mode == Some(config::DaemonMode::Daemon);
    if !want_daemon {
        // #167: el transporte embebido registra sus mutaciones (regla dura 4) en
        // EL journal del directorio de estado, el mismo que abre el daemon. Si
        // otro proceso tiene el lock exclusivo se sigue sin él, avisando — ver
        // `norte_core::embedded`.
        //
        // Construirlo NO abre el fichero (#177): un `ntc` que solo navega no le
        // quita el journal al daemon ni a un `norte audit`. El lock se toma en
        // la primera mutación, y el aviso —si lo hay— llega por el canal de
        // `take_journal_warnings`, ya dentro de la sesión.
        let engine = norte_core::embedded::engine_in(&norte_core::connect::config_dir());
        // #95.2: límites anti-bomba de archives desde `[archive]` (capas de
        // usuario, jamás la de proyecto). Antes de cualquier navegación: los
        // providers compuestos se cachean con los límites de su primer uso.
        // rust review item 3 (C1): la conversión override+saturación vivía
        // duplicada aquí y en `norte_core::archive_config` — un único home
        // en el core (`limits_from_overrides`) para que TUI y daemon jamás
        // diverjan en los límites anti-bomba.
        if let Some(limits) = norte_core::archive_config::limits_from_overrides(
            cfg.common.archive_max_entries,
            cfg.common.archive_max_decompressed_bytes,
            cfg.common.archive_max_nesting,
        ) {
            engine.set_archive_limits(limits);
        }
        // Ítem 11 del roadmap: el programa que lee los RAR, si la config fija
        // uno. Viene del mismo `cfg.common` que ya se cargó, así que no honra
        // la capa Project — y aquí eso no es una preferencia, es que un repo
        // ajeno no elige qué binario se lanza.
        engine.set_rar_delegate(
            cfg.common
                .archive_rar_delegate
                .as_ref()
                .map(std::path::PathBuf::from),
        );
        engine.register_provider(Arc::new(LocalProvider::os_root()));
        // Conexiones remotas (fase 6e): un path sftp://…/ftp://… navegable si
        // la host key ya es de confianza. La CONFIRMACIÓN TOFU interactiva
        // (modal con fingerprint) es UX pendiente — hoy un primer contacto
        // aparece como error con la huella; confírmalo con `norte connect`.
        engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
            norte_core::connect::config_dir(),
        )));
        // IA (M4-IA): opt-in; sin [ai] el backend degrada (Unsupported).
        //
        // Estos diagnósticos eran `eprintln!` y llevaban un comentario
        // explicando que un `tracing::warn!` aquí se descartaría mudo, porque
        // este binario no instalaba subscriber. Ya lo instala (`init_to_file`,
        // roadmap ítem 9), así que van al log como el resto — y sin escribir en
        // una pantalla que ratatui está a punto de tomar.
        match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
            Ok(Ok(ai_cfg)) => {
                if let Some(pcfg) = ai_cfg.rename_provider_config().cloned() {
                    match norte_core::ai::resolve_and_build(
                        &pcfg,
                        norte_core::connect::config_dir(),
                    )
                    .await
                    {
                        Ok(provider) => engine.set_ai_provider(provider),
                        Err(e) => tracing::warn!(error = %e, "proveedor de IA no disponible"),
                    }
                }
                // Embeddings (M4-IA-2): proveedor propio, opt-in igual —
                // future-proofing del plan: la TUI embebida aún no lleva
                // índice (with_index es del daemon), el wiring es por paridad
                // para cuando lo gane.
                if let Some(w) = norte_core::ai::install_embed_provider(&engine, &ai_cfg).await {
                    tracing::warn!(aviso = %w, "proveedor de embeddings");
                }
                engine.set_ai_config(ai_cfg);
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "[ai] inválido"),
            Err(e) => tracing::warn!(error = %e, "la carga de [ai] falló"),
        }
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    #[cfg(not(unix))]
    {
        let _ = (cli_socket, cfg);
        anyhow::bail!("el modo daemon no está disponible en Windows todavía (issue #33)");
    }
    #[cfg(unix)]
    {
        use norte_core::backend::remote::RemoteBackend;
        let socket = match cli_socket.or_else(|| cfg.common.daemon_socket.clone()) {
            Some(s) => s,
            None => tokio::task::spawn_blocking(|| norte_core::daemon::default_socket_path(None))
                .await
                .context("resolución del socket")?,
        };
        let exe = std::env::current_exe().context("current_exe")?;
        // El binario del daemon es `norte` (la CLI), no `norte-tui`: junto
        // al ejecutable actual dentro del mismo directorio de instalación.
        let daemon_bin = exe.with_file_name("norte");
        let mut spawn_cmd: Vec<std::ffi::OsString> =
            vec![daemon_bin.into(), "daemon".into(), "run".into()];
        spawn_cmd.push("--socket".into());
        spawn_cmd.push(socket.clone().into());
        let remote = RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                // El nombre del BINARIO, no el del crate: es lo que el daemon
                // registra y lo que un humano lee en un log o en `norte
                // doctor`, y ahí tiene que aparecer el programa que arrancó.
                name: "ntc".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

#[cfg(test)]
mod apply_cd_tests {
    use norte_frontend::layout::BySlot;
    use norte_tui::fill::{Fill, FillMsg};
    use norte_tui::jobs::SearchRun;
    use norte_tui::navigate::{Cd, apply_cd};
    use norte_tui::panel::{PaneSlots, SLOT_LEFT, SLOT_RIGHT};
    use norte_tui::probes::{DecorateFetch, Probed};

    /// Dos paneles de mentira: `apply_cd` solo les pregunta qué hueco ocupa
    /// cada posición.
    fn panes() -> PaneSlots {
        let d = norte_proto::VPath::parse("mem:///x").expect("wire");
        PaneSlots::new(
            norte_tui::app::Pane::new(d.clone(), Vec::new()),
            norte_tui::app::Pane::new(d, Vec::new()),
        )
    }

    fn hueco(i: usize) -> norte_frontend::layout::SlotId {
        if i == 0 { SLOT_LEFT } else { SLOT_RIGHT }
    }
    use norte_proto::Error;

    fn fill() -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { rx }
    }

    /// El hueco del pane 0 ocupado y el del 1 libre: la disposición de
    /// partida de casi todos estos casos.
    fn en_el_pane_0() -> BySlot<Fill> {
        let mut f = BySlot::new();
        f.insert(SLOT_LEFT, fill());
        f
    }

    /// Un REEMPLAZO del mismo pane suelta su relleno obsoleto.
    #[test]
    fn replaced_suelta_el_fill_del_pane() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(0));
        assert!(
            f.get(hueco(0)).is_none(),
            "el fill del listado viejo se suelta"
        );
    }

    /// Un reemplazo de OTRO pane no toca el relleno vivo.
    #[test]
    fn replaced_de_otro_pane_no_toca() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Replaced(1));
        assert!(f.get(hueco(0)).is_some(), "el fill del pane 0 sobrevive");
    }

    /// #78: un cd FALLIDO NO suelta el relleno — el pane sigue en su listado
    /// anterior, que se sigue rellenando (soltarlo lo colgaba en loading).
    #[test]
    fn failed_conserva_el_fill() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Failed(Error::NotFound),
        );
        assert!(
            f.get(hueco(0)).is_some(),
            "el fill del listado anterior sigue vivo tras un cd fallido"
        );
    }

    /// Un cd abandonado no toca nada.
    #[test]
    fn cancelled_conserva_el_fill() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(&panes(), &mut f, &mut df, &mut lp, &mut sr, Cd::Cancelled);
        assert!(f.get(hueco(0)).is_some());
    }

    /// Un listado nuevo ocupa el hueco DE SU PANE y solo ese: el relleno del
    /// otro pane sigue drenando. Es lo que hace que `pane.mirror` —que manda
    /// el OTRO pane a un sitio sin mover el foco— no pueda dejar a medias el
    /// pane que el lector está mirando.
    #[test]
    fn filling_de_un_pane_no_toca_el_hueco_del_otro() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Filling {
                pane: 1,
                fill: fill(),
            },
        );
        assert!(
            f.get(hueco(0)).is_some(),
            "el relleno del pane 0 sigue en su hueco"
        );
        assert!(f.get(hueco(1)).is_some(), "y el nuevo ocupa el suyo");
    }

    /// #118: Ctrl+R re-listó el pane 0 (listado COMPLETO nuevo) — su
    /// drenador viejo duplicaría filas si siguiera vivo. La dedup de la
    /// sonda #52 también caduca: el listado nuevo re-lazifica las entries.
    #[test]
    fn refreshed_suelta_el_fill_del_pane_relistado() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, false]),
        );
        assert!(
            f.get(hueco(0)).is_none(),
            "el drenador del listado viejo se suelta"
        );
        assert!(lp.is_empty(), "la dedup de la sonda #52 caduca");
    }

    /// #118: Esc a medias — el pane 1 NO llegó a re-listarse, su relleno
    /// paginado sigue siendo válido (#78: soltarlo lo colgaba en loading).
    #[test]
    fn refreshed_a_medias_conserva_el_fill_del_pane_no_relistado() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_RIGHT, fill());
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, false]),
        );
        assert!(
            f.get(hueco(1)).is_some(),
            "el fill del pane NO re-listado sobrevive al Esc a medias"
        );
    }

    /// Y el simétrico: un refresh de los DOS panes suelta los dos huecos.
    #[test]
    fn refreshed_de_ambos_panes_suelta_los_dos_huecos() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(SLOT_LEFT, fill());
        f.insert(SLOT_RIGHT, fill());
        let mut lp = Probed::new();
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([true, true]),
        );
        assert!(f.get(hueco(0)).is_none() && f.get(hueco(1)).is_none());
    }

    /// #118: refresh totalmente abandonado (Esc antes del primer pane) o
    /// ambos panes en modo virtual: nada cambió, nada se toca.
    #[test]
    fn refreshed_vacio_no_toca_nada() {
        let mut f = en_el_pane_0();
        let mut lp = Probed::from([(0, norte_proto::VPath::parse("file:///d/x").unwrap())]);
        let mut df: BySlot<DecorateFetch> = BySlot::new();
        let mut sr: Option<SearchRun> = None;
        apply_cd(
            &panes(),
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
            Cd::Refreshed([false, false]),
        );
        assert!(
            f.get(hueco(0)).is_some(),
            "sin pane re-listado, el fill sigue"
        );
        assert!(!lp.is_empty(), "sin pane re-listado, la dedup sigue");
    }
}

#[cfg(test)]
mod swap_tests {
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::SearchHits;
    use norte_tui::app::{App, Pane, SearchState};
    use norte_tui::event_loop::watch_targets;
    use norte_tui::fill::{Fill, FillMsg};
    use norte_tui::jobs::SearchRun;
    use norte_tui::navigate::{Cd, apply_cd, reconcile_swap};
    use norte_tui::probes::{DecorateFetch, Probed};
    use norte_tui::refresh::reap_search_run;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Una búsqueda viva CORRIENDO sobre `pane`. La Task es sintética (el
    /// `TaskRef` de test del core): aquí no se ejerce el walker, sino el
    /// índice de pane que el run loop guarda a su lado.
    fn search_run(pane: usize) -> SearchRun {
        let (_tx, rx) = tokio::sync::mpsc::channel::<SearchHits>(1);
        let id = norte_proto::TaskId::new(1);
        let (_progreso, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Search,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
        });
        SearchRun {
            task: TaskRef::synthetic_for_tests(id, prx),
            rx,
            pane,
            prev_dir: vp("file:///antes"),
            hits: 0,
            state: SearchState::Running,
        }
    }

    fn decorate(slot: norte_frontend::layout::SlotId) -> DecorateFetch {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        DecorateFetch {
            slot,
            dir: vp("mem:///d"),
            rx,
        }
    }

    /// El relleno EN VUELO está archivado POR PANE: si el intercambio no cruza
    /// los huecos, los lotes del listado siguen llegando al pane de al lado y
    /// el lector ve crecer la lista equivocada. Es el bug que una suite verde
    /// no ve, porque el listado sigue llegando: solo llega al sitio que no es.
    ///
    /// Comprueba que se movió ESE drenador y no un hueco cualquiera: el lote
    /// enviado por el `tx` del pane 0 se recoge del hueco del pane 1.
    #[test]
    fn el_intercambio_cruza_los_huecos_del_relleno_en_vuelo() {
        let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_LEFT, Fill { rx });
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        df.insert(
            norte_tui::panel::SLOT_LEFT,
            decorate(norte_tui::panel::SLOT_LEFT),
        );
        let mut lp = Probed::from([(0, vp("mem:///d/x"))]);
        let mut sr: Option<SearchRun> = None;

        reconcile_swap(
            norte_tui::panel::SLOT_LEFT,
            norte_tui::panel::SLOT_RIGHT,
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
        );

        assert!(
            f.get(norte_tui::panel::SLOT_LEFT).is_none(),
            "el hueco del pane 0 queda libre"
        );
        tx.try_send(FillMsg::Failed)
            .expect("el drenador sigue vivo");
        assert!(
            f.get_mut(norte_tui::panel::SLOT_RIGHT)
                .expect("cruzado al hueco del pane 1")
                .rx
                .try_recv()
                .is_ok(),
            "y es EL MISMO drenador el que ahora alimenta al pane 1"
        );
        assert!(
            df.get(norte_tui::panel::SLOT_RIGHT).is_some()
                && df.get(norte_tui::panel::SLOT_LEFT).is_none(),
            "cruzados"
        );
        assert!(lp.is_empty(), "la caché de stat se tira, no se traduce");
    }

    /// Sin nada en vuelo el reconciliado es inofensivo: un intercambio no
    /// puede inventar un relleno ni un fetch donde no los había.
    #[test]
    fn el_intercambio_sin_nada_en_vuelo_no_inventa_nada() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();
        let mut sr: Option<SearchRun> = None;
        reconcile_swap(
            norte_tui::panel::SLOT_LEFT,
            norte_tui::panel::SLOT_RIGHT,
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
        );
        assert!(
            f.get(norte_tui::panel::SLOT_LEFT).is_none()
                && f.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
        assert!(
            df.get(norte_tui::panel::SLOT_LEFT).is_none()
                && df.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
    }

    /// El desenlace `Cd::Swapped` tiene que LLEGAR al reconciliado: la mitad
    /// del intercambio que `dispatch` no puede hacer viaja por `apply_cd`, y
    /// un brazo que se olvidara de llamarlo dejaría el fill apuntando al pane
    /// que no es sin que ningún test de `App` se enterase.
    #[test]
    fn apply_cd_swapped_reconcilia_el_estado_del_run_loop() {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_RIGHT, Fill { rx });
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        df.insert(
            norte_tui::panel::SLOT_RIGHT,
            decorate(norte_tui::panel::SLOT_RIGHT),
        );
        let mut lp = Probed::from([(1, vp("mem:///d/x"))]);
        let mut sr: Option<SearchRun> = None;
        let panes = norte_tui::panel::PaneSlots::new(
            Pane::new(vp("mem:///d"), Vec::new()),
            Pane::new(vp("mem:///d"), Vec::new()),
        );
        apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
        assert!(
            f.get(norte_tui::panel::SLOT_LEFT).is_some()
                && f.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
        assert!(
            df.get(norte_tui::panel::SLOT_LEFT).is_some()
                && df.get(norte_tui::panel::SLOT_RIGHT).is_none()
        );
        assert!(lp.is_empty());
    }

    /// La búsqueda VIVA también está indexada por pane: `SearchRun` guarda el
    /// pane virtual que muestra los hits, exactamente como el relleno guarda
    /// el suyo. Si el intercambio no lo voltea, los hits siguen entrando en el
    /// pane de al lado.
    #[test]
    fn el_intercambio_voltea_el_pane_de_la_busqueda_viva() {
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();
        let mut sr = Some(search_run(0));

        reconcile_swap(
            norte_tui::panel::SLOT_LEFT,
            norte_tui::panel::SLOT_RIGHT,
            &mut f,
            &mut df,
            &mut lp,
            &mut sr,
        );

        assert_eq!(
            sr.as_ref().expect("el run sigue vivo").pane,
            1,
            "el pane virtual de la búsqueda cambió de lado con su pane"
        );
    }

    /// Y el intercambio no puede COSECHAR la búsqueda por el camino.
    ///
    /// El `Esc`/`Enter` del pane virtual son las ÚNICAS teclas que ese modo
    /// intercepta, así que un `Ctrl+U` cae al resolutor y cruza los panes con
    /// una búsqueda corriendo. Justo después, el mismo call site pasa por
    /// [`reap_search_run`], que suelta el run cuando su pane ya no es virtual:
    /// con el `pane` sin voltear mira el pane 0 —que ahora tiene el listado
    /// ordinario que vino del otro lado— y CANCELA la Task en silencio,
    /// dejando el pane 1 con hits a medias en `Running` para siempre y sin su
    /// manejador de `Esc` (que exige un run vivo PARA ESE pane).
    ///
    /// Por eso el volteo tiene que ocurrir DENTRO de `reconcile_swap`: pasada
    /// la cosecha ya no hay nada que salvar.
    #[test]
    fn un_intercambio_no_cosecha_la_busqueda_viva() {
        let mut app = App::new(
            Pane::new(vp("file:///izq"), Vec::new()),
            Pane::new(vp("file:///der"), Vec::new()),
        );
        // Búsqueda viva en el pane 0 (el `Alt+F7` lo dejó virtual).
        app.panes[0].begin_search(vp("file:///izq"));
        let mut sr = Some(search_run(0));
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut df: norte_frontend::layout::BySlot<DecorateFetch> =
            norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();

        // `Ctrl+U`: `dispatch` cruza los panes y el run loop reconcilia…
        app.swap_panes();
        let panes = norte_tui::panel::PaneSlots::new(
            Pane::new(vp("mem:///d"), Vec::new()),
            Pane::new(vp("mem:///d"), Vec::new()),
        );
        apply_cd(&panes, &mut f, &mut df, &mut lp, &mut sr, Cd::Swapped);
        // …y el MISMO call site cosecha a continuación.
        reap_search_run(&app, &mut sr);

        let s = sr.as_ref().expect("la búsqueda en curso NO se cancela");
        assert_eq!(s.pane, 1, "sigue los hits a su nuevo lado");
        assert!(
            app.panes[s.pane].virtual_search,
            "y ese lado es el que está en modo búsqueda"
        );
    }

    /// El watcher NO necesita reconciliado propio, y esto es lo que hace
    /// cierta esa afirmación: el conjunto vigilado se deriva de `app.panes`
    /// en cada vuelta del run loop (`rewatch(&watch_targets(app))` es la
    /// primera sentencia del bucle), así que basta con que `watch_targets`
    /// no cachee nada. Si alguien introdujera una copia por lado, el
    /// intercambio dejaría cada pane vigilando el dir del otro.
    #[test]
    fn watch_targets_sigue_a_los_panes_tras_el_intercambio() {
        let mut app = App::new(
            Pane::new(vp("file:///izq"), Vec::new()),
            Pane::new(vp("file:///der"), Vec::new()),
        );
        let antes = watch_targets(&app);
        app.swap_panes();
        let despues = watch_targets(&app);
        assert_eq!(
            antes[0], despues[1],
            "el dir izquierdo pasa a vigilarse a la derecha"
        );
        assert_eq!(antes[1], despues[0]);
        assert_ne!(antes[0], antes[1], "los dos dirs eran distintos de partida");
    }
}

#[cfg(test)]
mod refresh_ritual_tests {
    use norte_proto::VPath;
    use norte_tui::app::{App, Pane};
    use norte_tui::fill::{Fill, FillMsg};
    use norte_tui::jobs::SearchRun;
    use norte_tui::probes::Probed;
    use norte_tui::refresh::after_panes_refresh;

    fn fill() -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { rx }
    }

    fn app() -> App {
        let d = VPath::parse("file:///d").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// #118 (regresión pedida en el issue): un Esc a medias del refresh
    /// re-listó el pane 0 pero ABANDONÓ el 1 — el ritual solo puede soltar
    /// el drenador del pane re-listado de verdad; el del otro sigue drenando
    /// un listado que sigue siendo el suyo (#78).
    #[test]
    fn esc_a_medias_conserva_el_fill_del_pane_no_refrescado() {
        let mut app = app();
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        f.insert(norte_tui::panel::SLOT_RIGHT, fill());
        let mut lp = Probed::from([(1, VPath::parse("file:///d/x").unwrap())]);
        let mut sr: Option<SearchRun> = None;
        after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);
        assert!(
            f.get(norte_tui::panel::SLOT_RIGHT).is_some(),
            "el fill del pane 1 (no re-listado) sobrevive al Esc a medias"
        );
        assert!(lp.is_empty(), "la dedup de la sonda #52 caduca igualmente");
    }

    /// MAJOR-2: congelar impide que un veredicto cambie porque el lector se
    /// MUEVA, y eso está bien. Lo que no puede impedir es que cambie porque el
    /// MUNDO cambie: el brazo del `tick` no lleva guarda de overlay (a
    /// diferencia del de `dir_watch`, gateado por `watch_refresh_allowed`), así
    /// que una copia o un borrado que terminan con la ayuda abierta re-listan
    /// los dos panes y la entrada que los hechos describían puede haberse ido.
    /// La fila decía «no aplica a esta selección» de una selección que ya no
    /// existía.
    #[test]
    fn un_refresh_bajo_la_ayuda_abierta_recongela_los_hechos() {
        use norte_help::ChordResolver as _;

        let d = VPath::parse("file:///d").expect("wire de test");
        let fichero = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: d.join(norte_proto::Segment::new(b"leeme.txt".to_vec()).expect("segmento")),
            kind: norte_proto::EntryKind::File,
            size: Some(3),
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(d.clone(), vec![fichero]),
            Pane::new(d, Vec::new()),
        );
        norte_tui::overlays::open_contextual_help(&mut app, norte_help::Lang::En, &[], None);
        assert!(
            app.help_chords.availability("pane.view").is_available(),
            "con un fichero bajo el cursor, F3 se puede pulsar"
        );

        // La tarea termina, el refresh entra por debajo del overlay y se lleva
        // por delante la entrada de la que hablaban los hechos.
        app.panes[0].refresh_listing(Vec::new());
        let mut f: norte_frontend::layout::BySlot<Fill> = norte_frontend::layout::BySlot::new();
        let mut lp = Probed::new();
        let mut sr: Option<SearchRun> = None;
        after_panes_refresh(&mut app, [true, false], &mut f, &mut lp, &mut sr);

        assert_eq!(
            app.help_chords.availability("pane.view").reason(),
            Some(norte_help::Reason::WrongTarget),
            "el listado cambió: los hechos tienen que volver a congelarse"
        );
    }
}
