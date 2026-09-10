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
use norte_tui::screens::{apply_theme, drain_places_drives};
use norte_tui::session_push::restore_session;
use norte_tui::shortcuts_editor::build_keymaps;
use norte_tui::tty;
use norte_vfs_local::LocalProvider;
use std::sync::Arc;

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "wiring del binario, no API — mismo criterio que `run`/`dispatch`"
)]
async fn main() -> Result<()> {
    // Args: DIR posicional + `--preset`/`--daemon`/`--socket`. `--help` y
    // `--version` salen ANTES de tocar el terminal (antes se ignoraban como
    // flag desconocido y el binario moría al no poder abrir la TTY).
    let parsed = norte_frontend::cli::parse(std::env::args_os().skip(1), BOOL_FLAGS, VALUE_FLAGS);
    let Some(args) = args_or_exit(parsed)? else {
        return Ok(()); // `--help`/`--version`: ya impreso.
    };
    // `--setup` (spec 2026-09-10): volver a abrir el asistente de primer
    // arranque. Se lee ANTES de que `args` se desmonte por campos.
    let cli_setup = args.has("--setup");
    let (cli_preset, cli_layout, cli_profile, cli_daemon, cli_socket, cli_pick, cli_cd_file) = (
        args.text("--preset"),
        // `--layout` NO es texto por contrato: acaba siendo un nombre de
        // fichero, y por `to_string_lossy` dos bytes inválidos distintos
        // abrían el mismo `\u{FFFD}.toml` (#246 M1).
        args.os_text("--layout").map(std::ffi::OsString::from),
        // `--profile` tampoco es texto por contrato: acaba siendo
        // `profiles/<nombre>/`, que es un DIRECTORIO. Mismo motivo y mismo
        // #246 que `--layout`.
        args.os_text("--profile").map(std::ffi::OsString::from),
        args.has("--daemon"),
        args.path("--socket"),
        args.has("--pick"),
        args.path("--cd-file"),
    );
    // Un `--profile` EXPLÍCITO se conoce antes de conectar con nada, así que
    // entra en la PRIMERA carga y no por el cambio en caliente: así aplica
    // hasta `[ui] lang`, que es lo único que un cambio en marcha no puede
    // (`norte_i18n::force` corre una vez). El perfil PEGAJOSO no puede hacer
    // esto —vive en la sesión, y la sesión la tiene el daemon, al que se llega
    // con la config que estamos cargando— y por eso llega por el otro camino.
    //
    // Y si el que has nombrado no se puede usar, esto ABORTA (ADR 0079, D7):
    // pediste ese perfil, y arrancar como otra cosa sería contestar otra
    // pregunta. `load_async` ya nombra el fichero culpable.
    let layers = match &cli_profile {
        Some(name) => {
            // El nombre tiene que estar en el LISTADO, byte a byte. Mirar solo
            // si el resolutor produjo una capa NO basta: `splice` la añade en
            // cuanto el nombre es legal y hay dir de usuario, exista o no el
            // directorio — y entonces `load` la trata como una capa ausente,
            // que no es un error, y `--profile fantasma` arrancaba como si
            // nada. Que es justo lo que D7 declara fatal para un perfil que el
            // lector nombró.
            let dir = norte_config::profiles_dir_from(&|k| std::env::var_os(k))
                .context("no hay directorio de configuración donde colgar un perfil")?;
            let hay = norte_config::list_profiles(&dir)
                .unwrap_or_default()
                .iter()
                .any(|n| n == name);
            anyhow::ensure!(
                hay,
                "no hay ningún perfil que se llame «{}» en {}",
                name.to_string_lossy(),
                dir.display()
            );
            config::standard_layers_with_profile(Some(name))
        }
        None => config::standard_layers(),
    };
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
    // Y ADEMÁS a un anillo en memoria, que es lo que pinta `panel.log` (#323).
    // El fichero sirve para investigar después; el anillo, para ver lo que está
    // pasando sin salir de la TUI — que es donde se notó la falta: una conexión
    // que falla en 240 ms deja un «permiso denegado» que no dice nada mientras
    // el motivo exacto se escribe en un fichero de otra terminal.
    let log_ring = norte_core::logging::init_to_file_with_ring(
        norte_core::logging::LogConfig {
            dir: cfg.common.log_dir.as_deref(),
            retain: cfg.common.log_retain,
            // El fichero compartido: la CLI, el daemon y el terminal no
            // coinciden vivos sobre el mismo estado como sí lo hacen el daemon
            // y la ventana.
            prefix: None,
        },
        norte_config::logring::RING_DEFAULT,
    );
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
    let explicit_dir = args.dir.is_some();
    let start = start_dir(args.dir)?;
    // #108 b4: columnas y orden desde `[ui.columns]` — resuelto UNA vez;
    // los ids inválidos no rompen el arranque (doctor los reporta). ANTES de
    // los listados iniciales (#117): ellos también piden los attrs
    // configurados — sin esto, las celdas attr nacen en blanco hasta el
    // primer cd/refresh.
    let columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
        .with_date_format(cfg.common.ui_chrome.date_format());
    let start_attrs = columns.attr_ids_for(start.scheme());
    let left = initial_pane(&backend, &start, &start_attrs).await?;
    let right = initial_pane(&backend, &start, &start_attrs).await?;
    let mut app = App::new(left, right);
    // La revisión del binario, para la ayuda (F1). Vacía en los tests, que
    // construyen `App` sin pasar por aquí y hacen snapshots de la ayuda.
    app.version_line = norte_frontend::version::VERSION_LINE;
    // `None` si ya había subscriber: entonces nadie escribe en el anillo y el
    // panel lo DICE, en vez de enseñar un vacío que parece que no pasa nada.
    app.log_ring = log_ring;
    // Un `--profile` explícito ya está APLICADO (entró en las capas de la
    // primera carga), así que se declara activo aquí y no por el camino del
    // cambio en caliente. De paso es lo que hace que el perfil pegajoso de la
    // sesión no lo pise: el lector nombró uno para esta vez.
    app.active_profile.clone_from(&cli_profile);
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
    // Y si hay un daemon del que hablar (#328). Aquí y una sola vez, por lo
    // mismo que la línea de arriba: el `Backend` no cambia de brazo en vida del
    // proceso. Sin esto, un `ntc` corriente —sin daemon ninguno— abría el panel
    // de registro y su borde acababa diciendo «este daemon no sirve su
    // registro», que es una frase sobre alguien que no existe.
    app.log_remote.hay_daemon = backend.is_remote();
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
    // `[ui] menu_bar`: fijada salvo que se diga que no. Ausente = `true`,
    // que es lo contrario del criterio de casi todas las demás claves y es
    // deliberado: el menú era la única puerta a varios comandos y no había
    // nada en pantalla diciendo que existía.
    app.menu_bar = cfg.common.ui_menu_bar.unwrap_or(true);
    app.panel_bar = cfg.common.ui_panel_bar.unwrap_or(true);
    app.chrome = cfg.common.ui_chrome;
    app.set_parent_row(cfg.common.ui_parent_entry.unwrap_or(true));
    // `[ui] layout`: una disposición guardada. Un layout que no carga NO deja
    // a norte sin pantalla — se avisa por la barra y se arranca con
    // `orthodox`, que es lo que el usuario tenía antes de escribir la clave.
    // `--layout` gana a `[ui] layout`: elegir una disposición para UN arranque
    // no debe tocar tu config, que es justo lo que hace la clave.
    //
    // `orthodox` NO se filtra aquí. Se filtraba —el `App` ya nace con ese
    // árbol, así que cargarlo parecía trabajo de más— y eso hacía que un
    // `layouts/orthodox.toml` del usuario lo honrase la ventana y lo ignorase
    // el terminal: la misma capa de configuración diciendo dos cosas según
    // por dónde entres.
    let layout_name: Option<std::ffi::OsString> = cli_layout
        .clone()
        .or_else(|| cfg.common.ui_layout.clone().map(std::ffi::OsString::from));
    if let Some(name) = layout_name {
        // El fichero se lee FUERA del runtime (regla 2), y sin directorio de
        // config no hay fichero que valga: queda el preset de ese nombre.
        let loaded = match config::user_config_dir() {
            Some(dir) => {
                let n = name.clone();
                tokio::task::spawn_blocking(move || norte_frontend::layout::config::load(&dir, &n))
                    .await
                    .unwrap_or_else(|_| {
                        Err(norte_frontend::layout::LayoutError::NotFound(String::new()))
                    })
            }
            None => Err(norte_frontend::layout::LayoutError::NotFound(String::new())),
        };
        app.apply_loaded_layout(&name, loaded);
    }
    // `[ui] confirm_quit` también en el modelo: salir desde dentro de un
    // panel lateral lo decide `App`, no el despacho del run loop.
    app.confirm_quit = cfg.common.ui_confirm_quit;
    // L2: la pantalla que dejaste. Va DESPUÉS de `[ui] layout` a propósito —
    // una sesión guardada es más específica que una preferencia de config, y
    // es la que gana— y antes del tema, que no depende de ninguna de las dos.
    // Un fallo NO tumba el arranque: se sigue con la pantalla de la config.
    restore_session(
        &mut app,
        &backend,
        explicit_dir.then_some(&start),
        &cfg.common.profile_start,
    )
    .await;
    apply_theme(&mut app, &cfg);
    // Copia de la hotlist en el App (spec 2026-07-18): la fuente del popup
    // `Ctrl+D`; se refresca en cada hot-reload OK (`reload_config`).
    app.set_hotlist(cfg.common.hotlist.clone());
    // Si la disposición ya trae el sidebar —`full`, `explorer`, la sesión de
    // ayer—, montarla dejó las unidades pedidas. Se sirven AQUÍ y no en la
    // primera vuelta del bucle porque el bucle pinta antes de atender nada, y
    // el primer frame enseñaría la sección en blanco.
    drain_places_drives(&mut app, &backend).await;
    // Hints de pie de página de los overlays (H1 T3, #24): PRECOMPUTADOS del
    // efectivo `dialog` ANTES de que se mueva al `Resolver` de abajo — igual
    // que `help_lines`, se reconstruyen en cada hot-reload OK.
    app.dialog_hints = DialogHints::build(&dialog_eff);
    // `[ui] dialog_buttons` (spec 2026-09-10): la línea de teclas como
    // botones. Lo sabe la config, no el efectivo.
    app.dialog_hints.buttons = app.chrome.dialog_buttons();
    // La barra de teclas (spec 2026-09-10): de los TRES efectivos, aquí y en
    // cada hot-reload OK, por lo mismo que los hints.
    app.key_bars = norte_tui::app::KeyBars::build(&browse_eff, &viewer_eff, &dialog_eff);
    // #142: el acorde que devuelve los paneles, del MISMO efectivo y en el
    // mismo momento que lo de arriba. Si un rebind no llegara aquí, la tecla
    // que abre el subshell y la que lo cierra serían distintas.
    app.subshell_chord = norte_frontend::subshell::detach_chord(&browse_eff);
    // Openers declarativos (#28): fuente de `pane.open` (F4).
    app.openers = cfg.openers.clone();
    // `[ui] editor` (#133): el editor de norte, si la configuración nombra
    // uno. Sin él manda `$VISUAL`/`$EDITOR`, que es lo de siempre.
    app.editor = cfg
        .common
        .ui_editor
        .clone()
        .map(|command| norte_tui::app::EditorSpec {
            command,
            detached: cfg.common.ui_editor_detached.unwrap_or(false),
        });
    // `[ui] diff` (#312): el comparador de dos ficheros. Sin él, `diff -u`.
    app.diff = cfg
        .common
        .ui_diff
        .clone()
        .map(|command| norte_tui::app::EditorSpec {
            command,
            detached: cfg.common.ui_diff_detached.unwrap_or(false),
        });
    // Canales del modo daemon (None en embebido): tasks de otros frontends
    // y avisos de (re)conexión — se drenan en el loop principal.
    let foreign_tasks = backend.take_foreign_tasks();
    let conn_events = backend.take_conn_events();
    let approvals = backend.take_approvals();
    // #44: avisos `connection.degraded` del daemon → indicador persistente.
    let degraded = backend.take_degraded();
    // #322: avisos `connection.failed` → POR QUÉ no se pudo abrir una. Canal
    // aparte del de arriba y no un enum: son dos hechos distintos —una sesión
    // abierta que viaja mal, y una que no llegó a abrirse— y mezclarlos hace
    // que uno se pinte como el otro.
    let failed = backend.take_failed();
    // ADR 0100: lo que un plugin `hook` quiso decir sobre una mutación ya
    // registrada, o que sus hooks se apagaron. En embebido esto ARRANCA el
    // despachador de hooks sobre el journal de esta sesión.
    let plugin_notices = backend.take_plugin_notices();
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
    // El asistente de primer arranque (spec 2026-09-10): sin `norte.toml` de
    // usuario, o con `--setup`. Nunca bajo `--pick`.
    if norte_tui::wizard::should_open(cli_setup, cli_pick).await {
        norte_tui::wizard::open(&mut app, &cfg);
    }
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
    // Se vigilan TODOS los perfiles, no solo el activo.
    //
    // El vigilante se crea una vez, aquí, y cambiar de perfil en caliente
    // rehace las capas del bucle pero NO puede rehacerlo a él. Vigilando solo
    // las capas del arranque, un ajuste escrito en el perfil al que acabas de
    // cambiar —el tema, sin ir más lejos— no disparaba recarga: se guardaba en
    // el sitio correcto y la pantalla no cambiaba.
    //
    // Vigilarlos todos cuesta un watch por directorio y sobra para lo que hay
    // (un puñado de perfiles), y de paso hace que editar el fichero de un
    // perfil a mano recargue igual que editar el tuyo, que es lo que norte
    // promete de su configuración.
    let mut watch_dirs = layers.dirs.clone();
    if let Some(raiz) = norte_config::profiles_dir_from(&|k| std::env::var_os(k)) {
        for nombre in norte_config::list_profiles(&raiz).unwrap_or_default() {
            let dir = raiz.join(nombre);
            if !watch_dirs.iter().any(|(d, _)| *d == dir) {
                watch_dirs.push((dir, config::Layer::Profile));
            }
        }
    }
    let watch = config::watch(&config::Layers { dirs: watch_dirs }, cfg_tx).await;
    if watch.mode == WatchMode::Polling {
        app.message = Some(t("msg-config-polling"));
    }
    // Una capa de proyecto que no cargó (#260): antes del de Lua, que es el
    // que no puede quedar pisado.
    if !cfg.common.project_warnings.is_empty() {
        app.message = Some(ta(
            "msg-project-config-skipped",
            &[("n", &cfg.common.project_warnings.len().to_string())],
        ));
        for aviso in &cfg.common.project_warnings {
            tracing::warn!(motivo = %aviso, "capa de proyecto ignorada");
        }
    }
    // Y las líneas del PERFIL que no se entienden, con el mismo reparto: el
    // conteo a la barra, el motivo al registro. Se calculaban desde que hay
    // perfiles y no las enseñaba nadie, que es lo que dejaba a un
    // `[profile.start]` mal escrito sin sembrar y sin decirlo.
    //
    // DESPUÉS del de proyecto y ANTES del de Lua, que es el orden de la
    // ventana y el mismo criterio: el último gana, y el de seguridad —un
    // repositorio ajeno eligiendo qué código corre una tecla— es el que no
    // puede quedar pisado.
    if !cfg.common.profile_warnings.is_empty() {
        app.message = Some(ta(
            "msg-profile-config-ignored",
            &[
                (
                    "profile",
                    &app.active_profile
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                ("n", &cfg.common.profile_warnings.len().to_string()),
            ],
        ));
        for aviso in &cfg.common.profile_warnings {
            tracing::warn!(motivo = %aviso, "línea del perfil ignorada");
        }
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
        failed,
        plugin_notices,
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
        let (text, hostile) = norte_frontend::path_display(app.focused().dir());
        let marcado = if hostile { format!("!{text}") } else { text };
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
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut tty_out) = tty::open_controlling_terminal() {
            let _ = crossterm::execute!(tty_out, crossterm::event::DisableMouseCapture);
        }
        previous(info);
    }));
    let mut capture = mouse::Capture::new();
    if let Err(e) = capture.set(cfg.common.ui_mouse.unwrap_or(true), out) {
        tracing::warn!(error = %e, "no se pudo activar la captura de ratón");
        app.message = Some(t("msg-mouse-capture-failed"));
    }
    capture
}

/// Flags booleanos del TUI.
const BOOL_FLAGS: &[&str] = &["--daemon", "--pick", "--setup"];
/// Flags con valor del TUI.
const VALUE_FLAGS: &[&str] = &["--preset", "--layout", "--profile", "--socket", "--cd-file"];

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
      --profile <NAME>   Start in this profile — a directory under `profiles/`
                         in your config dir. Overrides the one you were last
                         in; refuses to start if it cannot be used
      --daemon           Talk to the daemon instead of the embedded core
      --socket <PATH>    Daemon socket (default: $XDG_RUNTIME_DIR/norte/daemon.sock)
      --pick             print the selection, NUL-terminated, and exit
      --cd-file PATH     write the final directory here, NUL-terminated
                         (used by the `norte shell-init` wrapper)
      --setup            Run the first-start wizard again (keys, theme, icons).
                         It also runs on its own when you have no norte.toml;
                         NORTE_NO_WIZARD=1 keeps it closed
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
    let native = match dir {
        Some(d) => {
            let meta = std::fs::metadata(&d)
                .with_context(|| format!("no se puede abrir {}", d.display()))?;
            anyhow::ensure!(meta.is_dir(), "{} no es un directorio", d.display());
            std::path::absolute(&d).unwrap_or(d)
        }
        None => std::env::current_dir().context("cwd")?,
    };
    norte_vfs_local::vpath_from_native(&native)
        .map_err(|e| anyhow::anyhow!("{} no representable como VPath: {e}", native.display()))
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
        println!("ntc {}", norte_frontend::version::VERSION_LINE);
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
        // El argv es el compartido: lo que arranca este terminal se apaga
        // solo cuando su último cliente se va.
        let spawn_cmd = norte_core::daemon::daemon_run_argv(daemon_bin, &socket);
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
mod tests {
    use super::{BOOL_FLAGS, USAGE, VALUE_FLAGS};

    /// Cada flag que este binario LEE está registrado, y sale en `--help`.
    ///
    /// Las tres listas son una sola cosa escrita tres veces —la tabla del
    /// parser, la lectura en `main`, y el texto de ayuda— y nada las ata.
    /// `--profile` se añadió leyéndolo en `main` y sin registrarlo, así que el
    /// parser lo rechazaba como desconocido: la bandera existía, el código que
    /// la usa existía, y `ntc --profile trabajo` contestaba «unknown flag».
    /// Ninguna suite lo vio; lo vio ejecutarlo.
    #[test]
    fn todo_flag_registrado_sale_en_la_ayuda() {
        for f in BOOL_FLAGS.iter().chain(VALUE_FLAGS.iter()) {
            assert!(USAGE.contains(f), "{f} está registrado y no sale en --help");
        }
    }

    /// Y al revés: cada flag largo que la ayuda promete está registrado, o el
    /// parser lo rechazará por desconocido justo cuando alguien lo copie de
    /// ahí.
    #[test]
    fn todo_flag_de_la_ayuda_esta_registrado() {
        let registrados: Vec<&str> = BOOL_FLAGS
            .iter()
            .chain(VALUE_FLAGS.iter())
            .copied()
            .collect();
        for linea in USAGE.lines() {
            for palabra in linea.split_whitespace() {
                let limpio = palabra.trim_end_matches(',');
                // `--help`/`--version` los sirve el propio parser, no estas
                // tablas.
                if limpio.starts_with("--")
                    && limpio.len() > 2
                    && !matches!(limpio, "--help" | "--version")
                {
                    assert!(
                        registrados.contains(&limpio),
                        "--help promete {limpio} y el parser no lo conoce"
                    );
                }
            }
        }
    }
}
