//! Binario del TUI (fases 3–4 M1): loop de eventos async sobre el core
//! EMBEBIDO o contra el DAEMON (fase 3 M2, por `[daemon] mode` o
//! `--daemon`), con keymap engine (ADR 0006). Regla 7: solo cambia el
//! transporte.
//! `ratatui::init/restore` gestionan raw mode + pantalla alternativa con
//! hook de pánico incluido: la terminal del usuario JAMÁS queda rota.
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use norte_core::backend::EntryStream;
use norte_core::backend::{Backend, ConnEvent, TaskRef};
use norte_core::{Engine, TransferOptions};
use norte_i18n::{t, ta};
use norte_proto::DeleteMode;
use norte_proto::methods::{FsSearchParams, SearchHits};
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_tui::app::{
    ALLOW_EXTENSIONS, ALLOW_NAV_HOTLIST, ALLOW_PICKER, ALLOW_PLUGIN_CONFIG, App, DialogOutcome,
    ExtensionManager, Help, KeymapsError, Modal, NavPopupKind, Palette, Pane, PendingWrite,
    PickerAction, SearchDialog, SearchState, Settings, SettingsEditError, TransferKind,
    config_error_category, detail_for_bar, dialog_action, error_category, error_message,
    io_error_category, keymaps_error_category, theme_error_category, trust_lua_key,
};
use norte_tui::config::{self, Layers, WatchMode};
use norte_tui::hints::DialogHints;
use norte_tui::keymap::{
    COMMANDS, DIALOG_COMMANDS, Effective, Resolution, Resolver, Screen, chord_from_crossterm,
    presets,
};
use norte_tui::lua::{
    CommandRun, Layer, LuaHost, PaneCtx, RunOutcome, StatusInput, TrustDecision, TrustStore,
};
use norte_tui::nav;
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use norte_vfs_local::LocalProvider;
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;

/// Filas que salta `cursor.page-up/down` (fijo hasta que el alto real del
/// pane viaje con el comando).
const PAGE: usize = 10;

/// Entradas de la PRIMERA página que un cd pinta antes de rellenar en
/// background (ADR 0017): con esto el primer render no espera al listado
/// entero (spec §11: primeras 100 en <16 ms aunque el dir tenga 500k).
const FIRST_PAGE: usize = 100;
/// Lote que el drenador coalesce antes de enviar (evita un re-sort por
/// entrada; el re-sort completo lo hace [`Pane::extend_listing`]). Un dir de
/// 100k son ~24 lotes ⇒ ~24 re-sorts de tamaño creciente durante el fill; el
/// merge incremental (claves persistidas) es la optimización diferida a issue.
const FILL_BATCH: usize = 4096;
/// El drenador vacía un lote PARCIAL cada tanto (además de al llenarlo): en un
/// listado remoto lento (páginas por RTT) el usuario ve progreso y el
/// contador `cargando… (n)` avanza en vez de saltar de 4096 en 4096.
const FILL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Tope por defecto de hits de una búsqueda viva (`Alt+F7`, liveSearch T6):
/// el diálogo v1 no expone el campo, así que se fija un tope razonable —
/// acota la memoria del pane virtual (los hits se acumulan en `entries`) y
/// hace alcanzable el estado `Truncated`. Al llegar, la Task completa y la
/// barra pinta «truncada».
const SEARCH_MAX_HITS: u32 = 10_000;

/// Una búsqueda viva EN CURSO (`Alt+F7`, liveSearch T6): la Task cancelable,
/// el canal de lotes de hits y el pane virtual que los muestra. Molde `Fill`:
/// vive en el run loop, se drena en el `select!` y se suelta al salir del modo
/// virtual (un `cd`) cancelando la Task (regla 3).
struct SearchRun {
    /// Task de `fs.search` (cancelable con `TaskRef::cancel`).
    task: TaskRef,
    /// Canal de lotes de hits (embebido: lo cierra el walker; remoto: la
    /// bomba del `RemoteBackend` lo cierra al terminal).
    rx: tokio::sync::mpsc::Receiver<SearchHits>,
    /// Pane que muestra los hits (índice en `App::panes`).
    pane: usize,
    /// Directorio ANTERIOR del pane, para restaurarlo al salir del modo
    /// virtual (Esc tras terminar).
    prev_dir: VPath,
    /// Hits acumulados (== `panes[pane].entries().len()`, contador propio para
    /// no depender del re-sort del pane).
    hits: usize,
    /// Estado del run: `Running` mientras el walker emite; terminal tras
    /// cerrarse el canal (se lee del `TaskProgress`).
    state: SearchState,
}

/// Mensaje del drenador de un listado paginado al run loop.
enum FillMsg {
    /// Un lote más de entradas para el pane.
    Batch(Vec<Entry>),
    /// El listado se cortó a mitad (error del provider/daemon): no es
    /// silencioso (la UI avisa y limpia el `loading`).
    Failed,
}

/// Un listado RELLENÁNDOSE en background: el pane destino y el canal del
/// drenador. Soltarlo (un cd nuevo) dropea el `rx` → el drenador muere en su
/// próximo envío → suelta el stream → cancelación cooperativa (regla 3).
struct Fill {
    pane: usize,
    rx: tokio::sync::mpsc::Receiver<FillMsg>,
}

/// Sonda one-shot de stat on-focus (#52): hidrata size/mtime de la entrada
/// seleccionada cuando el listado lazy los dejó en None. A lo sumo UNA en
/// vuelo; dedup por `(pane, path)` (un stat fallido no se reintenta hasta
/// cambiar la selección — sin martillear un provider roto). Acotada con
/// timeout (`STAT_PROBE_TIMEOUT`): un provider colgado no bloquea la sonda
/// para siempre.
struct StatProbe {
    pane: usize,
    path: VPath,
    rx: tokio::sync::oneshot::Receiver<Option<Entry>>,
}

/// Tope del stat de la sonda on-focus (#52, MINOR-1): un provider remoto
/// colgado no debe dejar la sonda en vuelo indefinidamente — vencido el
/// plazo se trata como fallo (entrada se queda en `None`, no se reintenta
/// hasta cambiar la selección).
const STAT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Lanza la sonda de `StatProbe`: clona el `Backend` (barato, Arc interno) y
/// el path para que la task no retenga el préstamo del run loop.
fn spawn_stat_probe(backend: &Backend, pane: usize, path: VPath) -> StatProbe {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    let p = path.clone();
    tokio::spawn(async move {
        let res = tokio::time::timeout(STAT_PROBE_TIMEOUT, b.stat(&p))
            .await
            .ok()
            .and_then(Result::ok);
        let _ = tx.send(res);
    });
    StatProbe { pane, path, rx }
}

/// Fetch de decoraciones de plugin EN VUELO (G3b, ADR 0037): el pane/dir
/// destino y el canal one-shot. Molde de [`StatProbe`] — a lo sumo UNO
/// global (mismo criterio simplificador que `fill`/`stat_probe`: un cd en
/// OTRO pane mientras este sigue en vuelo lo reemplaza, perdiendo esa
/// respuesta; documentado, no un bug — la próxima vez que se visite ese
/// pane se vuelve a pedir). `dir` se conserva para descartar una respuesta
/// TARDÍA que ya no corresponde al listado actual del pane (el usuario
/// cd'eó de nuevo antes de que el daemon respondiera).
struct DecorateFetch {
    pane: usize,
    dir: VPath,
    rx: tokio::sync::oneshot::Receiver<
        std::collections::HashMap<VPath, norte_frontend::Decoration>,
    >,
}

/// Lanza el fetch de decoraciones (G3b) para TODAS las entradas actualmente
/// listadas de `pane` (la "página visible" — el listado YA cargado, sea la
/// primera página de un dir grande paginándose o el dir entero; el resto de
/// un dir aún rellenándose queda sin decorar hasta la próxima visita, mismo
/// alcance MVP documentado en el ADR/plan). Sin guardia especial de "algún
/// decorator activado": `Backend::plugin_decorate` resuelve el catálogo en
/// cada llamada (barato embebido, una RPC remota) — intentar cada listado y
/// descartar en silencio si no hay decoradores consentidos es más simple y
/// honesto que cachear un flag que podría quedar obsoleto tras un F12.
/// `None` si el pane no tiene entradas (nada que decorar).
fn spawn_decorate_fetch(
    backend: &Backend,
    pane: usize,
    dir: VPath,
    paths: Vec<VPath>,
) -> Option<DecorateFetch> {
    if paths.is_empty() {
        return None;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let b = backend.clone();
    tokio::spawn(async move {
        let plugins = b.plugin_decorate(&paths).await.unwrap_or_default();
        let merged = norte_frontend::merge_decorations(&paths, &plugins);
        let _ = tx.send(merged);
    });
    Some(DecorateFetch { pane, dir, rx })
}

/// Pane que un desenlace de `cd` acaba de ASENTAR (`Filling`/`Replaced`,
/// listado nuevo YA en `app.panes[pane]`), o `None` si el cd no tocó ningún
/// pane (`Failed`/`Cancelled`). NO consume `outcome` (préstamo): el llamante
/// aún necesita pasarlo a [`apply_cd`] justo después.
fn cd_landed_pane(outcome: &Cd) -> Option<usize> {
    match outcome {
        Cd::Filling(f) => Some(f.pane),
        Cd::Replaced(pane) => Some(*pane),
        Cd::Failed(..) | Cd::Cancelled => None,
    }
}

/// Desenlace de un `cd`, para que el run loop actualice el relleno vivo.
enum Cd {
    /// El pane se reemplazó y su RESTO se rellena en background.
    Filling(Fill),
    /// El pane `usize` se reemplazó y ya está completo: un relleno anterior
    /// de ESE pane queda obsoleto y hay que soltarlo.
    Replaced(usize),
    /// El cd del pane `usize` FALLÓ al listar: el pane se quedó donde
    /// estaba (el error ya salió por la barra) sobre su listado ANTERIOR, así
    /// que un relleno previo de ese pane SIGUE siendo válido y se conserva
    /// (#78: soltarlo dejaba el pane colgado en `loading=true` —con
    /// «(parcial)» en la quick-search— sin drenador que lo apagara). El error
    /// VIAJA para quien navega desde el popup de historial (spec
    /// 2026-07-18: `NotFound` retira la entrada). Sin el índice de pane: al no
    /// tocar ya el relleno (#78) nadie lo consulta.
    Failed(Error),
    /// El cd se abandonó (Esc/Ctrl-C): nada cambió, el relleno sigue.
    Cancelled,
}

/// MINOR-4 (H1 close): un modal puede llegar de forma ASÍNCRONA (p. ej.
/// `Modal::ApproveAgentOp`, vía `ConnEvent` — un agente pide aprobación en
/// cualquier momento) mientras la palette está abierta. Sin este guard, el
/// run loop resolvía la tecla contra la palette PRIMERO (`app.palette.is_some()`
/// se comprobaba antes que `app.modal.is_some()`): un Enter pulsado para
/// responder al modal en realidad despachaba la fila resaltada de la
/// palette EN SILENCIO, y el modal de seguridad seguía esperando una
/// respuesta que nunca llegó por esa tecla. El modal SIEMPRE gana: la rama
/// de la palette del run loop excluye este caso de su condición (deja de
/// consumir la tecla) y la rama del modal cierra la palette, ahora obsoleta,
/// nada más entrar — la MISMA tecla cae al modal en la misma iteración.
#[must_use]
fn modal_preempts_palette(app: &App) -> bool {
    app.palette.is_some() && app.modal.is_some()
}

/// S3: el mismo guard que [`modal_preempts_palette`], para el overlay de
/// ajustes — un modal asíncrono (p.ej. una aprobación de policy) SIEMPRE
/// gana sobre `app.settings` abierto, igual razón: una tecla de respuesta al
/// modal no debe colarse como edición silenciosa de un ajuste.
#[must_use]
fn modal_preempts_settings(app: &App) -> bool {
    app.settings.is_some() && app.modal.is_some()
}

#[cfg(test)]
mod palette_modal_guard_tests {
    use super::*;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    fn approval_modal() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                ttl_ms: 60_000,
            },
        }
    }

    /// MINOR-4 (H1 close): con SOLO la palette abierta, no hay nada que
    /// preceder — el guard no dispara. Con AMBOS abiertos (un modal llegó
    /// asíncronamente encima de la palette), el modal debe ganar.
    #[test]
    fn modal_preempts_palette_solo_cuando_ambos_estan_abiertos() {
        let mut a = app();
        assert!(
            !modal_preempts_palette(&a),
            "sin overlays abiertos, nada que preceder"
        );
        a.palette = Some(Palette::new(Vec::new()));
        assert!(
            !modal_preempts_palette(&a),
            "solo la palette abierta: la palette maneja sus teclas normalmente"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_preempts_palette(&a),
            "un modal en vuelo con la palette abierta DEBE ganarle"
        );
    }

    /// S3: el mismo caso para `app.settings` — un modal en vuelo (p.ej. una
    /// aprobación de policy) gana sobre el overlay de ajustes abierto.
    #[test]
    fn modal_preempts_settings_solo_cuando_ambos_estan_abiertos() {
        let mut a = app();
        assert!(!modal_preempts_settings(&a));
        a.settings = Some(Settings::new(Vec::new()));
        assert!(
            !modal_preempts_settings(&a),
            "solo el overlay de ajustes abierto: maneja sus teclas normalmente"
        );
        a.modal = Some(approval_modal());
        assert!(
            modal_preempts_settings(&a),
            "un modal en vuelo con ajustes abierto DEBE ganarle"
        );
    }
}

/// Aplica el desenlace de un cd al relleno paginado en curso: uno nuevo lo
/// sustituye (el rx anterior dropeado mata su drenador → suelta el stream,
/// regla 3); un REEMPLAZO del MISMO pane lo suelta (su drenador drenaría el
/// listado viejo sobre el nuevo); un FALLO o un cd ABANDONADO no tocan el pane
/// —sigue en su listado anterior, cuyo relleno continúa siendo válido— así que
/// no tocan el fill (#78).
fn apply_cd(fill: &mut Option<Fill>, last_probed: &mut Option<(usize, VPath)>, outcome: Cd) {
    match outcome {
        Cd::Filling(f) => {
            // Listado nuevo (lazy): la dedup de la sonda #52 caduca — la
            // misma entrada re-enfocada debe poder re-hidratarse.
            *last_probed = None;
            *fill = Some(f);
        }
        Cd::Replaced(pane) => {
            *last_probed = None;
            if fill.as_ref().is_some_and(|f| f.pane == pane) {
                *fill = None;
            }
        }
        // El pane no cambió: su relleno (si lo había) sigue drenando el mismo
        // listado. Soltarlo aquí lo dejaba colgado en `loading=true` (#78).
        Cd::Failed(..) | Cd::Cancelled => {}
    }
}

/// Aplica un mensaje del drenador de paginación (ADR 0017) al pane. Si el pane
/// pasó a modo virtual de búsqueda (Alt+F7 sobre un dir aún paginándose,
/// review MAJOR T6), el fill quedó OBSOLETO —`begin_search` vació las
/// entries— y su drenador alimentaría el listado REAL como si fueran hits (el
/// propio root de la búsqueda colándose entre resultados): se suelta el fill y
/// se DESCARTA el lote. Cinturón simétrico al drain-guard de [`drain_search`];
/// el tirante es soltar el fill en `launch_search`.
fn apply_fill_msg(app: &mut App, fill: &mut Option<Fill>, pane: usize, msg: Option<FillMsg>) {
    if app.panes[pane].virtual_search {
        *fill = None;
        return;
    }
    match msg {
        Some(FillMsg::Batch(batch)) => app.panes[pane].extend_listing(batch),
        Some(FillMsg::Failed) => {
            app.panes[pane].finish_listing();
            app.message = Some(t("msg-list-incomplete"));
            *fill = None;
        }
        None => {
            app.panes[pane].finish_listing();
            *fill = None;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Args: preset posicional (`norte-tui vim`, capa MÁS alta sobre
    // norte.toml) + flags `--daemon`/`--socket` (fase 3 M2). Parseo a mano:
    // el TUI evita clap para no arrastrar su peso en el arranque.
    let (cli_preset, cli_daemon, cli_socket) = parse_args();
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

    let cwd = std::env::current_dir().context("cwd")?;
    // Un cwd UNC de Windows (\\server\share, \\wsl$\…) ya round-trip-ea:
    // vpath_from_native lo mete como primer segmento y to_native lo restituye
    // como base de la raíz del OS (#22). Un cwd irrepresentable aún da error
    // claro en vez de un panic.
    let start = norte_vfs_local::vpath_from_native(&cwd)
        .map_err(|e| anyhow::anyhow!("cwd no representable como VPath: {e}"))?;
    let left = Pane::new(
        start.clone(),
        backend
            .list(&start)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let right = Pane::new(
        start.clone(),
        backend
            .list(&start)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let mut app = App::new(left, right);
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
    let mut help_lines = norte_tui::help::build(&browse_eff, &viewer_eff);
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

    let mut terminal = ratatui::init();
    let res = run(
        &mut terminal,
        &mut app,
        &backend,
        &mut resolver,
        &mut viewer_resolver,
        &mut dialog_resolver,
        &mut help_lines,
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
    )
    .await;
    ratatui::restore();
    drop(watch);
    res
}

/// Parsea los argumentos: preset posicional + `--daemon`/`--socket`.
fn parse_args() -> (Option<String>, bool, Option<std::path::PathBuf>) {
    let mut preset = None;
    let mut daemon = false;
    let mut socket = None;
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let a = arg.to_string_lossy();
        match a.as_ref() {
            "--daemon" => daemon = true,
            "--socket" => {
                socket = it.next().map(std::path::PathBuf::from);
            }
            s if s.starts_with("--") => {} // flag desconocido: ignora (compat)
            _ if preset.is_none() => preset = Some(a.into_owned()),
            _ => {}
        }
    }
    (preset, daemon, socket)
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
        let engine = Engine::new();
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
        engine.register_provider(Arc::new(LocalProvider::os_root()));
        // Conexiones remotas (fase 6e): un path sftp://…/ftp://… navegable si
        // la host key ya es de confianza. La CONFIRMACIÓN TOFU interactiva
        // (modal con fingerprint) es UX pendiente — hoy un primer contacto
        // aparece como error con la huella; confírmalo con `norte connect`.
        engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
            norte_core::connect::config_dir(),
        )));
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
                name: "norte-tui".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

/// Resuelve el preset (flag > config > default) y pliega las capas de
/// keymap (ADR 0007) para las TRES pantallas (browse, viewer, dialog — H1
/// T2). El error tipado ([`KeymapsError`], #73) vive en `norte_tui::app`
/// junto a su categoría Fluent.
fn build_keymaps(
    cfg: &config::LoadedConfig,
    cli_preset: Option<&str>,
) -> Result<(Effective, Effective, Effective), KeymapsError> {
    let preset_name = cli_preset.unwrap_or(&cfg.common.preset);
    let presets = presets();
    let (_, preset) = presets
        .iter()
        .find(|(n, _)| *n == preset_name)
        .ok_or_else(|| KeymapsError::UnknownPreset {
            name: preset_name.to_owned(),
            available: presets
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", "),
        })?;
    let invalid = |e: norte_tui::keymap::KeymapError| KeymapsError::Invalid {
        detail: e.to_string(),
    };
    let browse = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse)
        .map_err(invalid)?;
    let viewer = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Viewer)
        .map_err(invalid)?;
    // Screen::Dialog fusiona `[dialog] ∪ [global]` (ADR 0006/H1 T1):
    // `build_for_impl` valida TODO el efectivo fusionado contra
    // `known_commands`, así que un binding GLOBAL (p. ej. `ctrl+c →
    // app.quit`) se validaría como `UnknownCommand` si solo pasáramos
    // `DIALOG_COMMANDS`. La UNIÓN con `COMMANDS` es la opción simple (T1 lo
    // deja elegido): inofensiva porque cada overlay ALLOWLISTEA solo sus
    // `dialog.*` soportados (`app::dialog_action` y las resoluciones ad hoc
    // de este módulo) y descarta cualquier otro comando resuelto.
    let dialog_known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let dialog = Effective::build_for(preset, &cfg.keymap_layers, &dialog_known, Screen::Dialog)
        .map_err(invalid)?;
    Ok((browse, viewer, dialog))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // wiring del binario, no API
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<String>,
    layers: Layers,
    cli_preset: Option<String>,
    // Modo del quick search (`[ui] quick_search`): vive en el run loop como
    // el preset CLI y se actualiza en el hot-reload de config.
    mut quick_mode: nav::Mode,
    // `[ui] confirm_quit` (S2): mismo patrón que `quick_mode` — vive en el
    // run loop, `applies_live` (solo afecta a `app.quit` NUEVOS, uno en
    // curso ya decidió) y se actualiza en el hot-reload.
    mut confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): la config COMPLETA vive aquí, no solo los campos
    // sueltos de arriba — el overlay de ajustes necesita leer CUALQUIER
    // entrada del catálogo (`crate::settings::build_rows`), no una lista
    // fija. Se actualiza ENTERA en cada hot-reload OK (`reload_config`, al
    // final, tras aplicar todo lo demás — mismo criterio que `quick_mode`/
    // `confirm_quit`: solo si TODO aplicó).
    mut cfg: config::LoadedConfig,
    mut cfg_rx: tokio::sync::mpsc::Receiver<()>,
    mut foreign_tasks: Option<tokio::sync::mpsc::UnboundedReceiver<norte_core::backend::TaskRef>>,
    mut conn_events: Option<tokio::sync::mpsc::UnboundedReceiver<ConnEvent>>,
    mut approvals: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>,
    >,
    mut degraded: Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>,
    >,
) -> Result<()> {
    let mut events = EventStream::new();
    // Tick del panel de tasks: copia snapshots del watch (jamás bloquea).
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Debounce del hot-reload SIN bloquear el loop (revisión fase 6): cada
    // evento de config empuja el deadline; el reload corre cuando vence.
    let mut reload_at: Option<tokio::time::Instant> = None;
    // Listado paginado rellenándose en background (ADR 0017): a lo sumo uno.
    let mut fill: Option<Fill> = None;
    // Búsqueda viva en curso (liveSearch T6): a lo sumo una (el pane virtual
    // es uno). Molde `Fill`: se drena en el select y se suelta al salir.
    let mut search_run: Option<SearchRun> = None;
    // Scripting Lua (M4, ADR 0026): host por capas con trust TOFU. Como
    // `fill`, el estado vive en el run loop. El run en vuelo (a lo sumo UNO:
    // el estado Lua es uno) se pollea inline en el select — `CommandRun` es
    // !Send y este future corre en block_on, jamás en spawn.
    let mut lua_host = load_lua(app, &layers).await;
    let mut lua_run: Option<(CommandRun, CancellationToken)> = None;
    let mut lua_queue: VecDeque<String> = VecDeque::new();
    // Sonda de stat on-focus (#52, listado lazy): a lo sumo una en vuelo,
    // dedup por (pane, path) — dos panes sobre el MISMO dir deben hidratar
    // cada uno la suya (no reintenta un stat fallido hasta cambiar
    // selección).
    let mut stat_probe: Option<StatProbe> = None;
    let mut last_probed: Option<(usize, VPath)> = None;
    // Fetch de decoraciones de plugin en vuelo (G3b, ADR 0037): a lo sumo
    // uno, molde de `stat_probe`/`fill`.
    let mut decorate_fetch: Option<DecorateFetch> = None;
    loop {
        // Barra Lua en cada vuelta, ANTES del draw (cacheada en el host).
        refresh_lua_status(app, lua_host.as_ref());
        // Exención puntual de la regla 2: el draw escribe stdout síncrono
        // (patrón async oficial de ratatui; acotado, runtime multi-thread).
        terminal.draw(|f| ui::draw(f, app))?;
        if app.quit {
            return Ok(());
        }
        // #52: listado lazy — la entrada enfocada sin size se hidrata con una
        // sonda one-shot (máx. una en vuelo; dedup por (pane, path)).
        if stat_probe.is_none()
            && let Some((pane_idx, path)) = app.focused_needs_stat()
            && last_probed.as_ref() != Some(&(pane_idx, path.clone()))
        {
            last_probed = Some((pane_idx, path.clone()));
            stat_probe = Some(spawn_stat_probe(backend, pane_idx, path));
        }
        tokio::select! {
            _ = tick.tick() => {
                // Un refresh de panes (mutación terminada) reescribe AMBOS
                // panes con el listado COMPLETO → suelta el relleno paginado
                // en curso (su drenador duplicaría entradas, BLOCKER del
                // rust-reviewer).
                if on_tick(app, backend, &mut events).await {
                    fill = None;
                    // Un refresh re-lazifica las entries (nuevo listado): la
                    // dedup previa quedaría bloqueando un re-probe legítimo
                    // de la MISMA selección (MAJOR-1).
                    last_probed = None;
                    // Un refresh de panes (refresh_listing) apaga el modo
                    // virtual del pane de búsqueda: suelta el run (su drenador
                    // alimentaría un listado real) y cancela si sigue vivo.
                    reap_search_run(app, &mut search_run);
                }
            }
            Some(task) = async {
                match &mut foreign_tasks {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Task de OTRO frontend de la misma sesión (fase 3): al panel.
                app.board.push_foreign(task);
            }
            Some(ev) = async {
                match &mut conn_events {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                app.message = Some(match ev {
                    ConnEvent::Lost => t("msg-daemon-lost"),
                    ConnEvent::Restored => t("msg-daemon-restored"),
                });
            }
            Some(req) = async {
                match &mut approvals {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Aprobación de policy pendiente (M3-3b T5): a la cola de
                // diálogos (jamás pisa un modal abierto) y se abre si procede.
                app.pending_approvals.push_back(req);
                app.open_next_pending();
            }
            Some(d) = async {
                match &mut degraded {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // #44: sesión remota degradó a texto plano — indicador
                // PERSISTENTE en la status bar (no pisa `message` transitorio).
                app.connection_warning = Some(ta(
                    "status-connection-degraded",
                    &[("scheme", d.scheme.as_str()), ("host", d.host.as_str())],
                ));
            }
            res = async {
                match &mut stat_probe {
                    Some(pr) => (&mut pr.rx).await.ok().flatten(),
                    None => std::future::pending().await,
                }
            } => {
                // Sonda de stat on-focus (#52): se limpia SIEMPRE (haya dado
                // `Some` o el stat fallara/canal se cerrara) — la dedup por
                // `last_probed` evita reintentar hasta cambiar la selección.
                if let Some(pr) = stat_probe.take()
                    && let Some(entry) = res
                {
                    app.panes[pr.pane].hydrate(&pr.path, entry.size, entry.mtime_ms);
                }
            }
            res = async {
                match &mut decorate_fetch {
                    Some(f) => (&mut f.rx).await.ok(),
                    None => std::future::pending().await,
                }
            } => {
                // Fetch de decoraciones (G3b): se limpia SIEMPRE. Una
                // respuesta tardía cuyo `dir` ya no case el del pane (el
                // usuario cd'eó de nuevo mientras estaba en vuelo) se
                // DESCARTA — nunca pinta badges de un listado que ya no se
                // ve (mismo criterio anti-stale que el drain-guard de
                // `apply_fill_msg` para búsqueda virtual).
                if let Some(f) = decorate_fetch.take()
                    && let Some(map) = res
                    && app.panes[f.pane].dir() == &f.dir
                {
                    app.panes[f.pane].set_decorations(map);
                }
            }
            msg = async {
                match &mut fill {
                    Some(f) => f.rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // Lote del drenador del listado paginado (ADR 0017): al pane
                // que lo abrió. `None` = canal cerrado (fin del drenado).
                let pane = fill.as_ref().map_or(0, |f| f.pane);
                apply_fill_msg(app, &mut fill, pane, msg);
            }
            hits = async {
                // Solo se drena mientras el run sigue vivo (`Running`): un
                // canal cerrado devolvería `None` en bucle (spin) — al leer el
                // `None` se pasa a terminal y este brazo queda pendiente.
                match &mut search_run {
                    Some(s) if s.state == SearchState::Running => s.rx.recv().await,
                    _ => std::future::pending().await,
                }
            } => {
                drain_search(app, &mut search_run, hits);
            }
            outcome = async {
                match &mut lua_run {
                    Some((run, _)) => run.await,
                    None => std::future::pending().await,
                }
            } => {
                // DROP INMEDIATO del CommandRun resuelto (contrato del
                // driver): retenerlo mantendría `run_active` encendido y la
                // statusbar Lua congelada.
                lua_run = None;
                match outcome {
                    RunOutcome::Ok { messages } => {
                        if !messages.is_empty() {
                            // Unidos con « · » y por detail_for_bar (tope +
                            // enmascarado): la barra es una línea.
                            app.message = Some(detail_for_bar(&messages.join(" · ")));
                        }
                    }
                    RunOutcome::Err { detail, .. } => {
                        app.message = Some(ta(
                            "err-lua-command",
                            &[("detail", &detail_for_bar(&detail))],
                        ));
                    }
                    RunOutcome::Cancelled => app.message = Some(t("err-lua-cancelled")),
                    RunOutcome::TimedOut => app.message = Some(t("err-lua-timeout")),
                }
                // FIFO: arranca el siguiente encolado. En bucle: si uno ya
                // no existe (hot-reload lo quitó → `err-lua-unknown`), el
                // resto de la cola no se queda atascado.
                if let Some(host) = lua_host.as_ref() {
                    while lua_run.is_none() {
                        let Some(next) = lua_queue.pop_front() else {
                            break;
                        };
                        lua_run = start_lua_run(app, host, backend, &next);
                    }
                }
            }
            Some(()) = cfg_rx.recv() => {
                // Ráfaga de guardados: empuja el deadline (ADR 0007).
                reload_at =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_millis(300));
            }
            () = async {
                match reload_at {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                reload_at = None;
                while cfg_rx.try_recv().is_ok() {}
                reload_config(
                    app,
                    backend,
                    resolver,
                    viewer_resolver,
                    dialog_resolver,
                    help_lines,
                    &layers,
                    cli_preset.as_deref(),
                    &mut quick_mode,
                    &mut confirm_quit,
                    &mut cfg,
                )
                .await;
                // Hot-reload del scripting Lua (ADR 0026): host NUEVO entero
                // (jamás estado a medias). Un `CommandRun` en vuelo retiene
                // el estado VIEJO vía sus handles clonados (documentado en
                // `lua::api`) y no se toca; statusbar/estado renacen. La
                // cola también: sus nombres apuntaban al registro viejo
                // (y si `load_lua` dio None, no quedaría quién drenarla).
                lua_host = load_lua(app, &layers).await;
                lua_queue.clear();
            }
            maybe = events.next() => {
                let Some(event) = maybe else { return Ok(()); };
                if let Event::Key(key) = event.context("evento de terminal")?
                    && key.kind == crossterm::event::KeyEventKind::Press
                {
                    app.message = None;
                    if app.theme_picker.is_some() {
                        on_theme_picker_key(app, dialog_resolver, key.modifiers, key.code).await;
                    } else if app.extensions.is_some() {
                        on_extensions_key(app, backend, dialog_resolver, key.modifiers, key.code)
                            .await;
                    } else if app.nav_popup.is_some() {
                        // Popup historial/hotlist (spec 2026-07-18): Enter
                        // sobre un item NAVEGA por el flujo de cd normal —
                        // su desenlace toca el relleno como cualquier cd.
                        let outcome = on_nav_popup_key(
                            app,
                            backend,
                            &mut events,
                            dialog_resolver,
                            key.modifiers,
                            key.code,
                        )
                        .await;
                        if let Some(pane) = cd_landed_pane(&outcome) {
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            decorate_fetch = spawn_decorate_fetch(backend, pane, dir, paths);
                        }
                        apply_cd(&mut fill, &mut last_probed, outcome);
                    } else if app.search_dialog.is_some() {
                        // Diálogo Alt+F7 (liveSearch T6): captura imprimibles
                        // como los demás overlays; Enter con criterio lanza la
                        // búsqueda (abre el pane virtual) — el resto de teclas
                        // no navegan.
                        if let Some(params) =
                            on_search_dialog_key(app, key.modifiers, key.code)
                        {
                            launch_search(app, backend, &mut fill, &mut search_run, params)
                                .await;
                        }
                    } else if app.palette.is_some() && !modal_preempts_palette(app) {
                        // Command palette (H1 T4): editor de filtro libre,
                        // como el diálogo de búsqueda de arriba — sus
                        // teclas son FIJAS, no resuelven por el contexto
                        // `dialog` (decisión 8 del plan H1: no hay
                        // vocabulario `dialog.*` para "teclear un carácter"
                        // o "correr la selección"). ctrl+c conserva su
                        // significado global (salir), como TODOS los
                        // overlays.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')
                        {
                            app.quit = true;
                            continue;
                        }
                        let plain = key.modifiers.is_empty()
                            || key.modifiers == KeyModifiers::SHIFT;
                        match key.code {
                            KeyCode::Char(c) if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.push_char(c);
                                }
                            }
                            KeyCode::Backspace if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.backspace();
                                }
                            }
                            KeyCode::Esc if plain => app.palette = None,
                            KeyCode::Up if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.up();
                                }
                            }
                            KeyCode::Down if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.down();
                                }
                            }
                            KeyCode::PageUp if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.page_up(PAGE);
                                }
                            }
                            KeyCode::PageDown if plain => {
                                if let Some(p) = &mut app.palette {
                                    p.page_down(PAGE);
                                }
                            }
                            KeyCode::Enter if plain => {
                                let cmd = app.palette.as_ref().and_then(Palette::selected);
                                app.palette = None;
                                if let Some(cmd) = cmd {
                                    // MISMA función de despacho que el
                                    // resolver del keymap invoca (#dispatch):
                                    // un comando elegido en la palette corre
                                    // EXACTAMENTE como si su tecla se
                                    // hubiera pulsado — incluida la apertura
                                    // de otro overlay (p.ej. `app.help`).
                                    let outcome = dispatch(
                                        app,
                                        backend,
                                        &mut events,
                                        help_lines,
                                        quick_mode,
                                        confirm_quit,
                                        &cfg,
                                        &cmd,
                                    )
                                    .await;
                                    if let Some(pane) = cd_landed_pane(&outcome) {
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            decorate_fetch = spawn_decorate_fetch(backend, pane, dir, paths);
                        }
                        apply_cd(&mut fill, &mut last_probed, outcome);
                                }
                            }
                            _ => {}
                        }
                    } else if app.settings.is_some() && !modal_preempts_settings(app) {
                        // Overlay de ajustes (S3): mismo criterio que la
                        // palette de arriba (decisión 8 del plan H1) — sus
                        // teclas son fijas, hardcodeadas en `on_settings_key`.
                        on_settings_key(app, key.modifiers, key.code).await;
                    } else if let Some(help) = &mut app.help {
                        // Teclas de la ayuda: fijas, como los diálogos (#24).
                        // ctrl+c conserva su significado global (salir).
                        match (key.modifiers, key.code) {
                            (KeyModifiers::CONTROL, KeyCode::Char('c')) => app.quit = true,
                            (
                                KeyModifiers::NONE,
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(1),
                            ) => app.help = None,
                            (KeyModifiers::NONE, KeyCode::Up) => help.scroll_up(1),
                            (KeyModifiers::NONE, KeyCode::Down) => help.scroll_down(1),
                            (KeyModifiers::NONE, KeyCode::PageUp) => help.scroll_up(PAGE),
                            (KeyModifiers::NONE, KeyCode::PageDown) => help.scroll_down(PAGE),
                            _ => {}
                        }
                    } else if app.modal.is_some() {
                        // MINOR-4 (H1 close): un modal llegado mientras la
                        // palette estaba abierta la cierra AQUÍ — obsoleta,
                        // y esta MISMA tecla responde al modal en vez de
                        // desaparecer dentro del filtro de la palette. El
                        // overlay de ajustes (S3) es el MISMO caso: un modal
                        // asíncrono (p.ej. una aprobación de policy) gana.
                        app.palette = None;
                        app.settings = None;
                        // El TOFU de Lua se resuelve AQUÍ (necesita el host,
                        // que vive en este loop): no navega ni toca `fill`.
                        if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
                            resolve_lua_trust(app, lua_host.as_ref(), key.code).await;
                            continue;
                        }
                        // ctrl+c conserva su significado global (salir),
                        // como los demás overlays (H1 T2) — ANTES de
                        // resolver contra el contexto `dialog`, hardcodeado.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')
                        {
                            app.quit = true;
                            continue;
                        }
                        // `Modal::MarkPattern` (#103 T9) es TEXTO libre, como
                        // el diálogo de búsqueda de arriba: consume
                        // caracteres crudos ANTES del contexto `dialog` — no
                        // tiene ALLOWLIST de `dialog_action` (`ctrl+c` ya
                        // quedó resuelto arriba, igual que para el resto de
                        // modales).
                        if matches!(app.modal, Some(Modal::MarkPattern { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.mark_pattern_push(c),
                                KeyCode::Backspace if plain => app.mark_pattern_pop(),
                                // Un `Err` deja el diagnóstico en el propio
                                // modal (`mark_pattern_confirm`, que lo deja
                                // abierto): nada más que hacer aquí.
                                KeyCode::Enter if plain => {
                                    if let Ok(n) = app.mark_pattern_confirm() {
                                        app.message = Some(ta(
                                            "msg-marked-by-pattern",
                                            &[("n", &n.to_string())],
                                        ));
                                    }
                                }
                                KeyCode::Esc if plain => app.close_modal(),
                                _ => {}
                            }
                            continue;
                        }
                        // El modal TOFU (#45) puede NAVEGAR al confiar: su Cd
                        // se aplica igual que el de un comando.
                        let outcome = on_dialog_key(
                            app,
                            backend,
                            &mut events,
                            dialog_resolver,
                            key.modifiers,
                            key.code,
                        )
                        .await;
                        if let Some(pane) = cd_landed_pane(&outcome) {
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            decorate_fetch = spawn_decorate_fetch(backend, pane, dir, paths);
                        }
                        apply_cd(&mut fill, &mut last_probed, outcome);
                    } else {
                        // Esc con un comando Lua en vuelo (BROWSE: sin modal
                        // ni overlay, y NO en el viewer): pide cancelación
                        // (regla 3) y CONSUME la tecla — no cae al resolver.
                        if app.viewer.is_none()
                            && key.modifiers.is_empty()
                            && key.code == KeyCode::Esc
                            && let Some((_, token)) = &lua_run
                        {
                            token.cancel();
                            continue;
                        }
                        // Pane virtual de búsqueda (liveSearch T6): con un
                        // search_run en el pane con foco (y sin quick vivo),
                        // Esc y Enter tienen semántica propia ANTES del
                        // resolver. El RESTO de teclas (cursor, F5/F8/F3…) cae
                        // al resolver y opera sobre el hit bajo el cursor.
                        if app.viewer.is_none()
                            && key.modifiers.is_empty()
                            && app.focused().quick().is_none()
                            && app.focused().virtual_search
                            && search_run
                                .as_ref()
                                .is_some_and(|s| s.pane == app.focus())
                        {
                            match key.code {
                                KeyCode::Esc => {
                                    // Task viva → cancela (hits conservados,
                                    // pasará a Cancelled al cerrarse el canal).
                                    // Ya terminada → sale del modo virtual
                                    // restaurando el dir anterior.
                                    on_search_escape(
                                        app,
                                        backend,
                                        &mut events,
                                        &mut fill,
                                        &mut last_probed,
                                        &mut search_run,
                                    )
                                    .await;
                                    continue;
                                }
                                KeyCode::Enter => {
                                    // Enter sobre un hit: cd al PADRE del hit y
                                    // cursor sobre él (sale del modo virtual).
                                    on_search_enter(
                                        app,
                                        backend,
                                        &mut events,
                                        &mut fill,
                                        &mut last_probed,
                                        &mut search_run,
                                    )
                                    .await;
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        // Quick search ACTIVO en el pane con foco (BROWSE):
                        // sus teclas se comen ANTES del resolver — un char
                        // (incluida otra `/`) alimenta la query y jamás
                        // re-entra al keymap (sin recursión). El RESTO de
                        // teclas (F5, F8, F3, Tab en Filter…) NO se consume:
                        // cae al resolver y opera sobre `selected()` ya
                        // filtrado — feed-to-listbox gratis. Decisión
                        // consciente: las teclas de navegación NO
                        // interceptadas (PageUp/PageDown/Home/End) también
                        // caen al resolver y mueven el CURSOR REAL, que con
                        // el filtro activo es invisible; al cancelar (Esc)
                        // reaparece donde lo dejaron. Conectarlas a la
                        // selección del filtro no compensa el estado extra.
                        if app.viewer.is_none() && app.focused().quick().is_some() {
                            let jump = app
                                .focused()
                                .quick()
                                .is_some_and(|q| q.mode() == nav::Mode::Jump);
                            // SHIFT pasa (una mayúscula llega como
                            // Char('A')+SHIFT y el char ya viene tal cual);
                            // ctrl/alt caen al resolver (ctrl+c sigue
                            // saliendo).
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => {
                                    app.focused_mut().quick_char(c);
                                    continue;
                                }
                                KeyCode::Backspace if plain => {
                                    app.focused_mut().quick_backspace();
                                    continue;
                                }
                                KeyCode::Up if plain => {
                                    app.focused_mut().quick_up();
                                    continue;
                                }
                                KeyCode::Down if plain => {
                                    app.focused_mut().quick_down();
                                    continue;
                                }
                                KeyCode::Tab if plain && jump => {
                                    app.focused_mut().quick_next();
                                    continue;
                                }
                                KeyCode::Esc if plain => {
                                    app.focused_mut().quick_cancel();
                                    continue;
                                }
                                KeyCode::Enter if plain => {
                                    // Confirma (cursor real = seleccionado) y
                                    // REUSA el camino de nav.enter: un dir (o
                                    // contenedor) entra, un fichero se queda.
                                    // `false` = el filtro no tenía matches:
                                    // solo cierra — jamás despachar sobre una
                                    // entrada que el usuario no veía (review
                                    // MAJOR T4).
                                    if app.focused_mut().quick_confirm() {
                                        let outcome = dispatch(
                                            app,
                                            backend,
                                            &mut events,
                                            help_lines,
                                            quick_mode,
                                            confirm_quit,
                                            &cfg,
                                            "nav.enter",
                                        )
                                        .await;
                                        if let Some(pane) = cd_landed_pane(&outcome) {
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            decorate_fetch = spawn_decorate_fetch(backend, pane, dir, paths);
                        }
                        apply_cd(&mut fill, &mut last_probed, outcome);
                                    }
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        // Pantalla activa: el viewer tiene su contexto.
                        let active = if app.viewer.is_some() {
                            &mut *viewer_resolver
                        } else {
                            &mut *resolver
                        };
                        // Teclas que el keymap no modela (Media, BackTab,
                        // CapsLock…) no llegan al resolver como chord, pero
                        // el trato SÍ es el mismo que un `Resolution::Reset`:
                        // `active.reset()` rompe cualquier secuencia
                        // pendiente EN EL RESOLVER (no solo el `app.pending`
                        // de pantalla) — antes `from_event` siempre empujaba
                        // un chord (aunque exótico) y el `Miss` resultante
                        // limpiaba el pending interno; `chord_from_crossterm`
                        // devuelve `None` en su lugar, así que el reset hay
                        // que pedirlo explícito, jamás dejar la secuencia a
                        // medias viva.
                        if let Some(chord) = chord_from_crossterm(key.modifiers, key.code) {
                            match active.push(chord) {
                                Resolution::Run(cmd) => {
                                    app.pending.clear();
                                    // `lua:<nombre>` (M4): al despachador Lua —
                                    // jamás a `dispatch` (no es comando fijo).
                                    if let Some(name) = cmd.strip_prefix("lua:") {
                                        run_lua_command(
                                            app,
                                            lua_host.as_ref(),
                                            backend,
                                            name,
                                            &mut lua_run,
                                            &mut lua_queue,
                                        );
                                        continue;
                                    }
                                    let outcome = dispatch(
                                        app,
                                        backend,
                                        &mut events,
                                        help_lines,
                                        quick_mode,
                                        confirm_quit,
                                        &cfg,
                                        &cmd,
                                    )
                                    .await;
                                    if let Some(pane) = cd_landed_pane(&outcome) {
                            let dir = app.panes[pane].dir().clone();
                            let paths: Vec<VPath> =
                                app.panes[pane].entries().iter().map(|e| e.path.clone()).collect();
                            decorate_fetch = spawn_decorate_fetch(backend, pane, dir, paths);
                        }
                        apply_cd(&mut fill, &mut last_probed, outcome);
                                    // Un cd (nav.parent…) apagó el modo virtual del
                                    // pane de búsqueda: suelta el run y cancela.
                                    reap_search_run(app, &mut search_run);
                                    // #28: `pane.open` dejó un comando externo
                                    // resuelto — el run loop (dueño de la
                                    // terminal) sondea el binario y lo lanza.
                                    if let Some((program, argv)) = app.pending_open.take() {
                                        app.message =
                                            Some(launch_opener(terminal, program, argv).await);
                                    }
                                }
                                Resolution::Pending(_) => {
                                    app.pending = active
                                        .pending()
                                        .iter()
                                        .map(ToString::to_string)
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                }
                                Resolution::Reset => app.pending.clear(),
                            }
                        } else {
                            active.reset();
                            app.pending.clear();
                        }
                    }
                }
            }
        }
        // Resize/Focus/etc: el draw del inicio del loop repinta solo.
    }
}

/// Hot-reload (ADR 0007): relee TODAS las capas; ante CUALQUIER error se
/// conserva la config vigente y se avisa por la barra — jamás romper una
/// sesión en marcha por un TOML a medio guardar.
/// Resuelve `[ui].theme` (preset o ruta) y lo aplica al `App`; ante error
/// degrada al preset por defecto y avisa (ADR 0020). El frontend no revienta
/// por un tema malo.
fn apply_theme(app: &mut App, cfg: &config::LoadedConfig) {
    let depth = norte_tui::theme::detect_depth();
    match norte_tui::theme::resolve(cfg.common.ui_theme.as_deref(), depth) {
        Ok(theme) => app.theme = theme,
        Err(e) => {
            app.theme = norte_tui::theme::TuiTheme::default();
            // Por categoría Fluent (#73): jamás el Display del OS ni el
            // diagnóstico crudo (el spec puede venir de un `./.norte` ajeno).
            app.message = Some(theme_error_category(&e));
        }
    }
}

/// Traduce las teclas del popup de tema a una acción de dominio (la lógica
/// vive en `App`, testeable) resolviendo contra el contexto `dialog` del
/// keymap (H1 T2, issue #24 — rebindeable). `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de resolver, como los demás overlays. `F9`
/// cierra el picker como atajo ESPECÍFICO de este overlay (no es un binding
/// `dialog.*` del preset): se mantiene hardcodeado. Al confirmar, PERSISTE
/// la elección en el `norte.toml` del usuario (ADR 0020), sin bloquear el
/// runtime.
async fn on_theme_picker_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if mods.is_empty() && code == KeyCode::F(9) {
        app.theme_picker_input(PickerAction::Cancel);
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        // Sin semántica de secuencia definida para overlays (T2): ignorar y
        // reiniciar el estado de resolución.
        Resolution::Pending(_) => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`) — una sola fuente para dispatch y
    // footer. El match sigue siendo exhaustivo por defensa en profundidad.
    if !ALLOW_PICKER.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este overlay: inerte
    }
    let action = match cmd.as_str() {
        "dialog.up" => PickerAction::Up,
        "dialog.down" => PickerAction::Down,
        "dialog.confirm" => PickerAction::Confirm,
        "dialog.cancel" => PickerAction::Cancel,
        _ => return, // ya filtrado por ALLOW_PICKER; inalcanzable en la práctica
    };
    // El nombre a persistir se toma ANTES de que Confirm cierre el popup.
    let confirmed = (action == PickerAction::Confirm)
        .then(|| {
            app.theme_picker
                .as_ref()
                .and_then(|p| p.selected().map(String::from))
        })
        .flatten();
    app.theme_picker_input(action);
    if let Some(name) = confirmed {
        // I/O en spawn_blocking: el runtime jamás se bloquea (regla 2).
        let n = name.clone();
        match tokio::task::spawn_blocking(move || config::persist_ui_theme(&n)).await {
            Ok(Ok(path)) => {
                // El path deriva de XDG_CONFIG_HOME/APPDATA (entorno):
                // saneado como cualquier detalle (#73).
                app.message = Some(ta(
                    "msg-theme-saved",
                    &[
                        ("name", &name),
                        ("path", &detail_for_bar(&path.display().to_string())),
                    ],
                ));
            }
            Ok(Err(e)) => {
                // El tema YA se aplicó (sesión); solo no se pudo guardar. A
                // la barra va la CATEGORÍA, jamás el Display del OS (#73).
                app.message = Some(ta(
                    "msg-theme-save-failed",
                    &[("error", &io_error_category(&e))],
                ));
            }
            // Un panic en el write es un bug nuestro: que no tumbe la TUI.
            Err(_) => {}
        }
    }
}

/// Qué hacer tras procesar una tecla del overlay de ajustes — separa el
/// cómputo PURO (dentro del borrow de `app.settings`, `on_settings_key`) del
/// I/O async (`persist_setting`, fuera de ese borrow): `Settings::activate`/
/// `edit_commit` no pueden devolver directamente y persistir en el mismo
/// paso porque ya toman `&mut app.settings` — separarlo en un enum evita
/// pedir prestado `app` dos veces a la vez.
enum SettingsKeyOutcome {
    /// La tecla se consumió sin nada que persistir (navegación/filtro/
    /// edición de buffer en curso).
    None,
    /// Esc fuera de edición: cierra el overlay.
    Close,
    /// Un ajuste cambió — persistir y anunciar. Boxed: `PendingWrite` lleva
    /// un `toml_edit::Value` propio y hace este brazo mucho más grande que
    /// el resto (clippy `large_enum_variant`) — indirección, no un tipo
    /// distinto.
    Write(Box<PendingWrite>),
    /// `Settings::edit_commit` rechazó el buffer — anunciar el error, sin
    /// tocar nada (el buffer se queda, `Settings` ya lo conserva).
    Invalid(SettingsEditError),
}

/// Teclas del overlay de ajustes (`app.settings`, S3): mismo criterio que la
/// palette (decisión 8 del plan H1) — editor de filtro libre, NO resuelve
/// por el contexto `dialog`; sus teclas quedan hardcodeadas aquí. `ctrl+c`
/// conserva su salida global. Mientras `Settings::is_editing()` las teclas
/// van al buffer de edición inline (mismo patrón que `name_input` del popup
/// de navegación: imprimibles/backspace crudos, Enter confirma, Esc
/// cancela); si no, navegan/filtran como la palette y Enter activa la fila
/// bajo el cursor (`Settings::activate` — cicla YA para `Bool`/`Enum`/
/// `ThemeName`/`PresetName`, o abre el buffer para `Text`/`Int`).
async fn on_settings_key(app: &mut App, mods: KeyModifiers, code: KeyCode) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    let outcome = {
        let Some(settings) = &mut app.settings else {
            return;
        };
        if settings.is_editing() {
            match code {
                KeyCode::Char(c) if plain => {
                    settings.edit_push_char(c);
                    SettingsKeyOutcome::None
                }
                KeyCode::Backspace if plain => {
                    settings.edit_backspace();
                    SettingsKeyOutcome::None
                }
                KeyCode::Esc => {
                    settings.edit_cancel();
                    SettingsKeyOutcome::None
                }
                KeyCode::Enter => match settings.edit_commit() {
                    Ok(write) => SettingsKeyOutcome::Write(Box::new(write)),
                    Err(e) => SettingsKeyOutcome::Invalid(e),
                },
                _ => SettingsKeyOutcome::None,
            }
        } else {
            match code {
                KeyCode::Char(c) if plain => {
                    settings.push_char(c);
                    SettingsKeyOutcome::None
                }
                KeyCode::Backspace if plain => {
                    settings.backspace();
                    SettingsKeyOutcome::None
                }
                KeyCode::Esc if plain => SettingsKeyOutcome::Close,
                KeyCode::Up if plain => {
                    settings.up();
                    SettingsKeyOutcome::None
                }
                KeyCode::Down if plain => {
                    settings.down();
                    SettingsKeyOutcome::None
                }
                KeyCode::PageUp if plain => {
                    settings.page_up(PAGE);
                    SettingsKeyOutcome::None
                }
                KeyCode::PageDown if plain => {
                    settings.page_down(PAGE);
                    SettingsKeyOutcome::None
                }
                KeyCode::Enter if plain => {
                    // Listas VIVAS para `ThemeName`/`PresetName` (mismo
                    // criterio que `App::open_theme_picker`): resueltas aquí,
                    // no `&'static` — el tema/keymap efectivo puede cambiar
                    // en caliente.
                    let theme_names: Vec<String> = norte_theme::preset_names()
                        .into_iter()
                        .map(String::from)
                        .collect();
                    let all_presets = presets();
                    let preset_names: Vec<&str> = all_presets.iter().map(|(n, _)| *n).collect();
                    match settings.activate(&theme_names, &preset_names) {
                        Some(write) => SettingsKeyOutcome::Write(Box::new(write)),
                        None => SettingsKeyOutcome::None,
                    }
                }
                _ => SettingsKeyOutcome::None,
            }
        }
    };
    match outcome {
        SettingsKeyOutcome::None => {}
        SettingsKeyOutcome::Close => app.settings = None,
        SettingsKeyOutcome::Write(write) => persist_setting(app, *write).await,
        SettingsKeyOutcome::Invalid(e) => app.message = Some(settings_edit_error_message(&e)),
    }
}

/// Persiste un [`PendingWrite`] (S3) — `spawn_blocking` (regla 2), mismo
/// patrón que el persist del theme picker (`on_theme_picker_key` arriba):
/// resuelve `user_config_dir()` a mano en vez de reutilizar
/// `config::persist_ui_theme` (esa wrapper no toma `section`/`key` — S2 solo
/// dio el genérico `persist_set(dir, ...)` con `dir` explícito).
async fn persist_setting(app: &mut App, write: PendingWrite) {
    let Some(dir) = config::user_config_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let PendingWrite {
        section,
        key,
        value,
        name,
        display,
    } = write;
    match tokio::task::spawn_blocking(move || config::persist_set(&dir, section, &key, value)).await
    {
        Ok(Ok(_path)) => {
            app.message = Some(ta(
                "msg-settings-saved",
                &[("name", &name), ("value", &display)],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Revisión S I1: la tarea de `spawn_blocking` panicó o se canceló
        // (antes: silencio total — la fila optimista de `Settings::
        // commit_row` quedaba MINTIENDO "editado" aunque nada se escribió).
        // No debe tumbar la TUI: se anuncia en la barra (categoría genérica,
        // sin `{$error}` — un `JoinError` no trae una categoría limpia) y se
        // deja rastro con `tracing` para diagnóstico — jamás `eprintln!`
        // aquí, que corrompería la pantalla alterna de ratatui mientras la
        // TUI sigue viva.
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_setting no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}

/// Mensaje de barra para un [`SettingsEditError`] (S3) — por CATEGORÍA
/// Fluent, nunca texto ad hoc (#73 pattern). Envoltorio fino (revisión S,
/// M6): byte-idéntico al de la GUI (`settings_view::edit_error_message`) —
/// hoisteado a [`norte_frontend::settings::edit_error_message`].
fn settings_edit_error_message(e: &SettingsEditError) -> String {
    norte_frontend::settings::edit_error_message(e)
}

#[cfg(test)]
mod settings_message_tests {
    use norte_i18n::{Lang, t_in};

    /// Revisión S I1: `msg-settings-save-crashed` (el brazo `Err(_)` de
    /// `persist_setting`, ver su doc) resuelve a texto REAL en ambos
    /// locales — no al id crudo, que es lo que se vería en la barra si
    /// faltara la clave en algún `.ftl`. Mismo criterio de cobertura que
    /// `norte_frontend::settings`'s `fluent_keys_existen_en_ambos_locales_
    /// para_cada_entrada`.
    #[test]
    fn msg_settings_save_crashed_existe_en_ambos_locales() {
        for lang in [Lang::Es, Lang::En] {
            assert_ne!(
                t_in(lang, "msg-settings-save-crashed"),
                "msg-settings-save-crashed",
                "falta la clave en {lang:?}"
            );
        }
    }
}

/// Teclas del overlay de extensiones (M4-P3), resueltas contra el contexto
/// `dialog` del keymap (H1 T2, issue #24); `ctrl+c` conserva su salida
/// global, hardcodeado ANTES de resolver. Regla 7: aprobar/activar viaja al
/// core por el `Backend`; el bool LOCAL solo se togglea tras un OK (feedback
/// inmediato sin relistar). El id y el estado se toman ANTES del `.await`
/// (el borrow del `mgr` se suelta durante la llamada al backend y se
/// re-obtiene después para reflejar el resultado). Allowlist de este
/// overlay: `dialog.up/down/cancel/approve/toggle-enabled` — `approve`
/// togglea la APROBACIÓN del plugin (decisión 3 del plan H1: "aprobar un
/// plugin" reutiliza semánticamente `dialog.approve`, antes era la tecla
/// `a` hardcodeada; ahora `a` es `dialog.add`, que este overlay no soporta).
async fn on_extensions_key(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if app.extensions.is_none() {
        return;
    }
    // G3c drill-down: while a `[config]` `string`/`int` edit buffer is
    // active, keys are captured RAW (same idiom as `on_nav_popup_key`'s
    // `name_input`) — bypassing the keymap resolver entirely, so typing
    // e.g. "y" edits the buffer instead of resolving to `dialog.approve`.
    let editing = app
        .extensions
        .as_ref()
        .and_then(|m| m.config.as_ref())
        .is_some_and(|p| p.state.is_editing());
    if editing {
        on_plugin_config_edit_key(app, backend, mods, code).await;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        Resolution::Pending(_) => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    let panel_open = app.extensions.as_ref().is_some_and(|m| m.config.is_some());
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`) — una sola fuente para dispatch y footer.
    // G3c: qué allowlist aplica depende de si el panel de `[config]` está
    // abierto.
    let allow: &[&str] = if panel_open {
        ALLOW_PLUGIN_CONFIG
    } else {
        ALLOW_EXTENSIONS
    };
    if !allow.contains(&cmd.as_str()) {
        return; // fuera del allowlist de este contexto: inerte
    }
    if panel_open {
        on_plugin_config_panel_cmd(app, backend, &cmd).await;
    } else {
        on_extensions_list_cmd(app, backend, &cmd).await;
    }
}

/// G3c: teclas RAW mientras un `string`/`int` de `[config]` se edita
/// (`on_extensions_key`'s guard `editing`) — mismo idioma que
/// `on_nav_popup_key`'s `name_input`.
async fn on_plugin_config_edit_key(
    app: &mut App,
    backend: &Backend,
    mods: KeyModifiers,
    code: KeyCode,
) {
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    match code {
        KeyCode::Char(c) if plain => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_push_char(c);
            }
        }
        KeyCode::Backspace if plain => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_backspace();
            }
        }
        KeyCode::Esc => {
            if let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) {
                panel.state.edit_cancel();
            }
        }
        KeyCode::Enter => {
            let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
                return;
            };
            match panel.state.edit_commit() {
                Ok(write) => {
                    let id = panel.plugin_id.clone();
                    commit_plugin_config_write(app, backend, &id, write).await;
                }
                Err(err) => {
                    app.message = Some(norte_frontend::settings::edit_error_message(&err));
                }
            }
        }
        _ => {}
    }
}

/// G3c: comandos resueltos (`up`/`down`/`confirm`/`cancel`) mientras el
/// panel de `[config]` está abierto y NADA se edita (`on_extensions_key`,
/// `panel_open` branch — `allow == ALLOW_PLUGIN_CONFIG`).
async fn on_plugin_config_panel_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(panel) = app.extensions.as_mut().and_then(|m| m.config.as_mut()) else {
        return;
    };
    match cmd {
        "dialog.up" => panel.state.up(),
        "dialog.down" => panel.state.down(),
        "dialog.cancel" => {
            if let Some(mgr) = &mut app.extensions {
                mgr.config = None;
            }
        }
        "dialog.confirm" => {
            if let Some(write) = panel.state.activate() {
                let id = panel.plugin_id.clone();
                commit_plugin_config_write(app, backend, &id, write).await;
            }
        }
        _ => {}
    }
}

/// El resto de `on_extensions_key`: comandos sobre la LISTA de plugins
/// (`panel_open == false`, `allow == ALLOW_EXTENSIONS`) — navegar,
/// aprobar/activar, y `dialog.confirm` (G3c) abre el panel de `[config]`
/// del plugin resaltado SI declara alguna clave. Enter NUNCA aprueba (pin
/// P1): solo entra en un submenú.
async fn on_extensions_list_cmd(app: &mut App, backend: &Backend, cmd: &str) {
    let Some(mgr) = &mut app.extensions else {
        return;
    };
    match cmd {
        "dialog.up" => mgr.up(),
        "dialog.down" => mgr.down(),
        "dialog.cancel" => app.extensions = None,
        "dialog.approve" => {
            // Id y estado ANTES del await (suelta el borrow de `mgr`).
            let Some((id, cur)) = mgr.selected().map(|p| (p.id.clone(), p.approved)) else {
                return;
            };
            match backend.plugins_set_approval(&id, !cur).await {
                Ok(()) => {
                    if let Some(mgr) = &mut app.extensions {
                        mgr.set_local_approved(!cur);
                    }
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        "dialog.toggle-enabled" => {
            let Some((id, cur)) = mgr.selected().map(|p| (p.id.clone(), p.enabled)) else {
                return;
            };
            match backend.plugins_set_enabled(&id, !cur).await {
                Ok(()) => {
                    if let Some(mgr) = &mut app.extensions {
                        mgr.set_local_enabled(!cur);
                    }
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        "dialog.confirm" => {
            let Some((id, name)) = mgr.selected().map(|p| (p.id.clone(), p.name.clone())) else {
                return;
            };
            match backend.plugin_get_config(&id).await {
                Ok(result) if !result.keys.is_empty() => {
                    let rows = norte_frontend::plugin_config::sanitize_config_keys(&result.keys);
                    let (plugin_name, _) = norte_tui::app::display_name(name.as_bytes());
                    if let Some(mgr) = &mut app.extensions {
                        mgr.config = Some(norte_tui::app::PluginConfigPanel {
                            plugin_id: id,
                            plugin_name,
                            state: norte_frontend::plugin_config::PluginConfigState::new(rows),
                        });
                    }
                }
                Ok(_) => app.message = Some(t("msg-plugin-config-empty")),
                Err(e) => app.message = Some(error_message(&e)),
            }
        }
        _ => {} // fuera del allowlist de este overlay: inerte
    }
}

/// Persiste UN [`norte_frontend::plugin_config::PendingConfigWrite`] vía
/// `Backend::plugin_set_config` y anuncia el resultado (G3c) — factorizado
/// fuera de [`on_extensions_key`] porque el mismo commit ocurre desde DOS
/// sitios (edición inline confirmada con Enter, y un `bool`/`enum` que
/// cicla de inmediato en `dialog.confirm`).
async fn commit_plugin_config_write(
    app: &mut App,
    backend: &Backend,
    plugin_id: &str,
    write: norte_frontend::plugin_config::PendingConfigWrite,
) {
    match backend
        .plugin_set_config(plugin_id, &write.key, &write.value)
        .await
    {
        Ok(()) => {
            app.message = Some(ta(
                "msg-plugin-config-saved",
                &[("key", &write.key), ("value", &write.display)],
            ));
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Teclas del popup de navegación (historial `Alt+↓` / hotlist `Ctrl+D`);
/// `ctrl+c` conserva su salida global, hardcodeado ANTES de nada. Con
/// `name_input` activo (el `a` de hotlist abre un campo para el nombre del
/// favorito) los imprimibles/backspace se capturan como editor de texto RAW
/// — H1 T2 decisión: NO es un comando `dialog.*`, es entrada libre, se
/// queda hardcodeado. Fuera de `name_input`, la tecla resuelve contra el
/// contexto `dialog` del keymap (H1 T2, issue #24); `add`/`remove` los
/// filtra el ALLOWLIST de este overlay a `kind == Hotlist` (el historial no
/// tiene nada que nombrar ni borrar — mismo criterio que antes de H1).
/// Enter sobre un item válido NAVEGA por el flujo de cd normal; si el cd
/// desde el HISTORIAL falla con `NotFound`, la entrada se retira (spec
/// 2026-07-18) — la de hotlist NO (es config del usuario: se avisa y queda).
async fn on_nav_popup_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(popup) = &mut app.nav_popup else {
        return Cd::Cancelled;
    };
    let kind = popup.kind;
    // SHIFT pasa (mayúsculas llegan como Char+SHIFT); ctrl/alt no escriben.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if popup.name_input.is_some() {
        match code {
            KeyCode::Char(c) if plain => {
                if let Some(input) = &mut popup.name_input {
                    input.push(c);
                }
            }
            KeyCode::Backspace if plain => {
                if let Some(input) = &mut popup.name_input {
                    input.pop();
                }
            }
            KeyCode::Esc => popup.name_input = None,
            KeyCode::Enter => {
                let name = popup.name_input.take().unwrap_or_default();
                // Input vacío = cancela (plan T5): no hay favorito sin nombre.
                if !name.is_empty() {
                    hotlist_add(app, &name).await;
                }
            }
            _ => {}
        }
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        Resolution::Pending(_) => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H1 T3: el MISMO allowlist que consume el hint generado
    // (`hints::DialogHints::build`, campo `nav_list`) — una sola fuente
    // para dispatch y footer. Cubre AMBOS kinds (History es un subconjunto:
    // `add`/`remove` los filtra el guard `kind == Hotlist` de más abajo).
    if !ALLOW_NAV_HOTLIST.contains(&cmd.as_str()) {
        return Cd::Cancelled; // fuera del allowlist de este overlay: inerte
    }
    match cmd.as_str() {
        "dialog.up" => {
            app.nav_popup_input(PickerAction::Up);
        }
        "dialog.down" => {
            app.nav_popup_input(PickerAction::Down);
        }
        "dialog.cancel" => {
            app.nav_popup_input(PickerAction::Cancel);
        }
        "dialog.add" if kind == NavPopupKind::Hotlist => {
            app.nav_popup_open_name_input();
        }
        "dialog.remove" if kind == NavPopupKind::Hotlist => {
            if let Some(name) = app.nav_popup_selected_hotlist_name() {
                hotlist_remove(app, &name).await;
            }
        }
        "dialog.confirm" => {
            // Confirm sobre un item inválido/vacío es no-op (el popup sigue).
            if let Some(path) = app.nav_popup_input(PickerAction::Confirm) {
                let pane = app.focus();
                let outcome = cd(app, backend, events, path.clone()).await;
                if kind == NavPopupKind::History && matches!(&outcome, Cd::Failed(Error::NotFound))
                {
                    // El dir ya no existe: fuera del historial. La barra ya
                    // muestra el error normal del cd fallido.
                    app.history[pane].remove(&path);
                }
                return outcome;
            }
        }
        _ => {} // fuera del allowlist de este overlay (o kind): inerte
    }
    Cd::Cancelled
}

/// `config::user_config_dir()` o el MISMO io `NotFound` que fabrica
/// `persist_ui_theme` sin entorno (CI pelada): la barra lo pinta como
/// `err-not-found` vía categoría (#73), clave existente y razonable.
fn user_config_dir_io() -> std::io::Result<std::path::PathBuf> {
    config::user_config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sin directorio de config de usuario",
        )
    })
}

/// Persiste el favorito `name` = cwd del pane con foco en el `norte.toml`
/// del USUARIO (`spawn_blocking`, regla 2 — `persist_hotlist_add` es
/// bloqueante por contrato). Solo si el disco fue bien se refresca la copia
/// en `App` (consistencia con disco) y sale `msg-hotlist-saved`; un fallo
/// io sale por categoría y la copia NO se toca.
async fn hotlist_add(app: &mut App, name: &str) {
    let target = app.focused().dir().clone();
    let wire = target.to_wire();
    let n = name.to_owned();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = user_config_dir_io()?;
        config::persist_hotlist_add(&dir, &n, &wire)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_saved(name, target);
            // El name lo tecleó el usuario, pero un PASTE puede colar
            // bidi/controles: por `detail_for_bar` como todo detalle (#73).
            app.message = Some(ta("msg-hotlist-saved", &[("name", &detail_for_bar(name))]));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // Un panic al persistir es un bug NUESTRO: que reviente visible
        // (criterio del binario, mismo que `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Retira el favorito `name` del `norte.toml` del USUARIO (`spawn_blocking`,
/// regla 2). Mismo contrato de consistencia que [`hotlist_add`].
async fn hotlist_remove(app: &mut App, name: &str) {
    let n = name.to_owned();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = user_config_dir_io()?;
        config::persist_hotlist_remove(&dir, &n)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_removed(name);
            app.message = Some(ta(
                "msg-hotlist-removed",
                &[("name", &detail_for_bar(name))],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

#[allow(clippy::too_many_arguments)] // wiring del hot-reload, no API
async fn reload_config(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    viewer_resolver: &mut Resolver,
    dialog_resolver: &mut Resolver,
    help_lines: &mut Vec<String>,
    layers: &Layers,
    cli_preset: Option<&str>,
    quick_mode: &mut nav::Mode,
    confirm_quit: &mut config::ConfirmQuit,
    // S3 (`app.settings`): la snapshot COMPLETA que `run()` retiene para
    // construir/refrescar el overlay de ajustes — reemplazada ENTERA solo
    // si TODO el reload aplicó (mismo criterio que el resto de esta
    // función); un reload fallido deja la config VIGENTE, jamás a medias.
    cfg_out: &mut config::LoadedConfig,
) {
    match config::load_async(layers.clone()).await {
        Ok(cfg) => match build_keymaps(&cfg, cli_preset) {
            Ok((browse, viewer, dialog)) => {
                // El modo del quick search sigue a la config vigente (solo
                // afecta a quick searches NUEVOS; uno abierto conserva el
                // suyo). Mismo criterio que el tema: solo si TODO aplicó.
                *quick_mode = cfg.quick_search_mode;
                // `[ui] confirm_quit` (S2): mismo criterio — solo afecta a
                // `app.quit` NUEVOS (uno ya abierto como `Modal::ConfirmQuit`
                // conserva su decisión hasta que el usuario responda).
                *confirm_quit = cfg.common.ui_confirm_quit;
                // La copia de hotlist también (un popup abierto conserva su
                // snapshot hasta reabrirse — items congelados a propósito).
                app.hotlist.clone_from(&cfg.common.hotlist);
                // Openers (#28): recargados con el resto de la config.
                app.openers = cfg.openers.clone();
                // Bindings `lua:` descartados del keymap de PROYECTO
                // (seguridad — mismo aviso que en el arranque; máximo
                // porque `global` se fusiona en las tres pantallas, H1 T2
                // suma dialog).
                let discarded_lua = browse
                    .discarded_lua_bindings()
                    .max(viewer.discarded_lua_bindings())
                    .max(dialog.discarded_lua_bindings());
                // La ayuda refleja el keymap VIGENTE: se reconstruye aquí.
                *help_lines = norte_tui::help::build(&browse, &viewer);
                app.help = None;
                // Filas de la palette (H1 T4): reconstruidas del keymap
                // VIGENTE, ANTES de que se mueva al resolver de abajo —
                // mismo criterio que help_lines. La palette abierta se
                // cierra (como la ayuda): sus filas congeladas podrían
                // apuntar a descripciones/chords ya viejos.
                app.palette_rows = norte_tui::palette::build_rows(&browse, &viewer);
                app.palette = None;
                // Hints de los overlays (H1 T3, #24): reconstruidos del
                // efectivo `dialog` VIGENTE, ANTES de que se mueva al
                // resolver de abajo — mismo criterio que help_lines.
                app.dialog_hints = DialogHints::build(&dialog);
                *resolver = Resolver::new(browse);
                *viewer_resolver = Resolver::new(viewer);
                *dialog_resolver = Resolver::new(dialog);
                app.pending.clear();
                app.message = Some(t("msg-config-reloaded"));
                // El tema también es hot-reloadable (ADR 0020): si falla, el
                // mensaje de error del tema pisa el de "config recargada".
                apply_theme(app, &cfg);
                // ÚLTIMO: el aviso de seguridad no debe quedar pisado.
                if discarded_lua > 0 {
                    app.message = Some(ta(
                        "msg-lua-keymap-project",
                        &[("n", &discarded_lua.to_string())],
                    ));
                }
                // S3: el overlay de ajustes, si está abierto, se REFRESCA
                // (no se cierra como `help`/`palette` arriba) — sus filas son
                // solo `(nombre, descripción, valor)` leídas de `cfg`, seguras
                // de recomputar sin tirar el filtro/edición en curso del
                // usuario (`Settings::refresh`).
                if let Some(settings) = &mut app.settings {
                    let summaries = plugin_config_summaries(backend).await;
                    settings.refresh(norte_tui::settings::build_rows(&cfg, &summaries));
                }
                *cfg_out = cfg;
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-config-not-applied",
                    &[("error", &keymaps_error_category(&e))],
                ));
            }
        },
        Err(e) => {
            app.message = Some(ta(
                "msg-config-not-applied",
                &[("error", &config_error_category(&e))],
            ));
        }
    }
}

/// Tope de la cola FIFO de comandos Lua (M4): con un run en vuelo, los
/// siguientes se encolan hasta aquí; llena, solo queda el aviso.
const LUA_QUEUE_MAX: usize = 8;

/// Directorio de ESTADO del usuario: `$XDG_STATE_HOME/norte` o
/// `~/.local/state/norte` (Windows: `%LOCALAPPDATA%\norte\state`). Sin
/// precedente en el workspace (verificado 2026-07-18: ningún uso de
/// `XDG_STATE_HOME`; la config usa `XDG_CONFIG_HOME` —
/// `config::user_config_dir`): el trust store de Lua es ESTADO local de la
/// máquina, no config que deba viajar con los dotfiles. `None` si el
/// entorno no define nada (CI pelada): el caller degrada con aviso
/// (fail-closed para el TOFU — sin store no corre el script de proyecto).
fn state_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("norte").join("state"));
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("norte"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/norte"))
}

/// Etiqueta ESTABLE de una capa para `err-lua-load` (no localizada: es un
/// identificador de capa, no prosa).
fn lua_layer_label(layer: Layer) -> &'static str {
    match layer {
        Layer::System => "system",
        Layer::User => "user",
        Layer::Project => "project",
    }
}

/// Evalúa una capa en el host y enruta error/warnings a la barra
/// (`err-lua-load`; con varios, el último gana el hueco — ok v1). El
/// detalle es diagnóstico CRUDO del runtime Lua: SIEMPRE por
/// `detail_for_bar` (patrón #73).
fn eval_lua_layer(app: &mut App, host: &LuaHost, source: &[u8], layer: Layer) {
    let label = lua_layer_label(layer);
    match host.eval_layer(source, layer) {
        Ok(warnings) => {
            for w in warnings {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", label), ("detail", &detail_for_bar(&w.detail))],
                ));
            }
        }
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", label),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
        }
    }
}

/// Lee `path` si existe (`spawn_blocking`, regla 2): `None` = capa ausente.
async fn read_optional_bytes(path: std::path::PathBuf) -> std::io::Result<Option<Vec<u8>>> {
    match tokio::task::spawn_blocking(move || std::fs::read(&path)).await {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(Err(e)) => Err(e),
        // Un panic leyendo es un bug NUESTRO: que reviente visible (mismo
        // criterio que `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Carga los `init.lua` por capas (ADR 0007 + 0026): sistema y usuario se
/// evalúan directo (config PROPIA del usuario); el de PROYECTO (`./.norte`,
/// la ÚLTIMA capa, como en `config::standard_layers`) pasa por el trust
/// TOFU ([`load_lua_project`]). La carga NO toca el `Backend` (solo evalúa
/// código; el FS de los comandos llega en `invoke`). Errores/warnings van a
/// la barra por categoría; una capa rota no impide las demás.
///
/// Devuelve `None` si mlua no pudo ni arrancar: el scripting queda
/// deshabilitado con aviso — el TUI sigue.
///
/// En hot-reload se llama de nuevo y el host RENACE entero (un `CommandRun`
/// en vuelo retiene el estado viejo vía sus handles — documentado en
/// `lua::api`); un `TrustLuaInit` pendiente de la carga anterior queda
/// obsoleto y se cierra (sus bytes ya no son lo que se evaluaría).
async fn load_lua(app: &mut App, layers: &Layers) -> Option<LuaHost> {
    if matches!(app.modal, Some(Modal::TrustLuaInit { .. })) {
        app.modal = None;
        app.open_next_pending();
    }
    app.lua_pending_trust = None;

    let host = match LuaHost::new() {
        Ok(h) => h,
        Err(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "host"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return None;
        }
    };
    for &(ref dir, layer) in &layers.dirs {
        // El kind viaja POR DIR (deuda #75 cerrada): antes se infería por
        // posición y el LABEL fallaba en Windows sin ProgramData (APPDATA
        // quedaba "system").
        if layer == Layer::Project {
            load_lua_project(app, &host, dir.clone()).await;
        } else {
            match read_optional_bytes(dir.join("init.lua")).await {
                Ok(Some(bytes)) => eval_lua_layer(app, &host, &bytes, layer),
                Ok(None) => {}
                Err(e) => {
                    app.message = Some(ta(
                        "err-lua-load",
                        &[
                            ("layer", lua_layer_label(layer)),
                            ("detail", &io_error_category(&e)),
                        ],
                    ));
                }
            }
        }
    }
    Some(host)
}

/// Resultado de la lectura VERIFICADA del `init.lua` de proyecto.
enum ProjectLua {
    /// No hay `./.norte/init.lua` (o `.norte` no es un directorio): nada.
    Absent,
    /// `.norte` o `init.lua` son SYMLINKS (criterio de seguridad de la
    /// review de T6): un symlink a un proyecto ya trusted ejecutaría
    /// contenido aprobado para OTRO sitio en un contexto hostil. No se
    /// carga, con aviso.
    Symlink,
    /// io real (permisos, etc.).
    Io(std::io::Error),
    /// Path CANÓNICO + bytes leídos UNA sola vez.
    Ready(std::path::PathBuf, Vec<u8>),
}

/// Chequeos + lectura del script de proyecto, todo síncrono en un bloque
/// (se llama bajo `spawn_blocking`): `symlink_metadata` verifica que `.norte`
/// es directorio REAL y que `init.lua` es fichero REGULAR — jamás a través
/// de un symlink. La ventana entre check y `read` no es cero (no hay
/// `O_NOFOLLOW` portable aquí), pero el contenido LEÍDO es exactamente lo que
/// se aprueba y evalúa (anti-TOCTOU del contenido; el residual es del path).
/// El path se CANONICALIZA para que check y record usen siempre la misma
/// forma (limitación NFC/NFD del store, documentada en `lua::trust`).
fn project_lua_read(dir: &std::path::Path) -> ProjectLua {
    let dir_md = match std::fs::symlink_metadata(dir) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if dir_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !dir_md.is_dir() {
        return ProjectLua::Absent;
    }
    let file = dir.join("init.lua");
    let file_md = match std::fs::symlink_metadata(&file) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ProjectLua::Absent,
        Err(e) => return ProjectLua::Io(e),
    };
    if file_md.file_type().is_symlink() {
        return ProjectLua::Symlink;
    }
    if !file_md.is_file() {
        return ProjectLua::Absent;
    }
    let canon = match std::fs::canonicalize(&file) {
        Ok(c) => c,
        Err(e) => return ProjectLua::Io(e),
    };
    match std::fs::read(&file) {
        Ok(bytes) => ProjectLua::Ready(canon, bytes),
        Err(e) => ProjectLua::Io(e),
    }
}

/// Capa de PROYECTO (ADR 0026): verifica symlinks, consulta el
/// [`TrustStore`] (`state_dir()/lua-trust.toml`, `spawn_blocking`) y decide
/// — `Trusted` evalúa; `Denied` CALLA; `DeniedPathChanged` avisa (deny
/// silencioso, JAMÁS modal automático: reabrirlo en cada edición de un
/// script ya rechazado acabaría en aprobación por fatiga); Unknown abre el
/// modal TOFU dejando los bytes pendientes en `App::lua_pending_trust`.
async fn load_lua_project(app: &mut App, host: &LuaHost, dir: std::path::PathBuf) {
    let read = match tokio::task::spawn_blocking(move || project_lua_read(&dir)).await {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    let (path, bytes) = match read {
        ProjectLua::Absent => return,
        ProjectLua::Symlink => {
            app.message = Some(t("msg-lua-symlink"));
            return;
        }
        ProjectLua::Io(e) => {
            app.message = Some(ta(
                "err-lua-load",
                &[("layer", "project"), ("detail", &io_error_category(&e))],
            ));
            return;
        }
        ProjectLua::Ready(path, bytes) => (path, bytes),
    };
    let Some(state) = state_dir() else {
        // Sin dir de estado no hay store; sin store no hay TOFU; sin TOFU el
        // script de proyecto NO corre (fail-closed) — con aviso.
        app.message = Some(t("err-lua-no-state-dir"));
        return;
    };
    let store_path = state.join("lua-trust.toml");
    let (check_path, check_bytes) = (path.clone(), bytes.clone());
    let decision = match tokio::task::spawn_blocking(move || {
        TrustStore::open(store_path).map(|s| s.check(&check_path, &check_bytes))
    })
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            // Store ilegible/corrupto: fail-closed (podría ser el rastro de
            // una manipulación, no una ausencia benigna — `lua::trust`).
            app.message = Some(ta(
                "err-lua-load",
                &[
                    ("layer", "project"),
                    ("detail", &detail_for_bar(&e.to_string())),
                ],
            ));
            return;
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    };
    match decision {
        TrustDecision::Trusted => eval_lua_layer(app, host, &bytes, Layer::Project),
        TrustDecision::Denied => {}
        TrustDecision::DeniedPathChanged => app.message = Some(t("msg-lua-denied-changed")),
        TrustDecision::Unknown => {
            if app.modal.is_some() {
                // Otro modal abierto (solo alcanzable en hot-reload): ni se
                // pisa ni se encola (v1) — el próximo reload re-pregunta.
                return;
            }
            let hash = sha2::Sha256::digest(&bytes);
            // 16 bytes = 32 hex = 128 bits (security review M4 Lua): el
            // humano compara LO QUE VE — forjar una colisión de 32 bits
            // (8 hex) cuesta minutos; 128 bits es imposible en la práctica.
            let hash_abbrev = hash.iter().take(16).fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            });
            app.modal = Some(Modal::TrustLuaInit {
                // Saneado AQUÍ (contrato del modal: `path` ya listo para
                // pintar) — un path de repo ajeno puede traer bidi/control.
                path: detail_for_bar(&path.display().to_string()),
                hash_abbrev,
            });
            app.lua_pending_trust = Some((path, bytes));
        }
    }
}

/// Resuelve el modal [`Modal::TrustLuaInit`] (interceptado en el run loop,
/// que es quien tiene el host — decisión 8 del plan H1: NO migrado al
/// contexto `dialog`): `y` confía, `n`/Esc deniegan, Enter NO decide
/// ([`trust_lua_key`]). La decisión se PERSISTE en el [`TrustStore`]
/// (`spawn_blocking`, regla 2) y, si aprueba, se evalúan los BYTES guardados
/// en `App::lua_pending_trust` — lo aprobado = lo evaluado (anti-TOCTOU),
/// jamás una relectura de disco. Un fallo al persistir no bloquea la
/// decisión de ESTA sesión (solo re-preguntará la próxima): aviso y sigue.
async fn resolve_lua_trust(app: &mut App, host: Option<&LuaHost>, code: KeyCode) {
    if app.modal.is_none() {
        return;
    }
    let allow = match trust_lua_key(code) {
        DialogOutcome::Confirmed => true,
        DialogOutcome::Cancelled => false,
        DialogOutcome::Open | DialogOutcome::Retry(_) => return,
    };
    app.modal = None;
    if let Some((path, bytes)) = app.lua_pending_trust.take() {
        let (rec_path, rec_bytes) = (path.clone(), bytes.clone());
        let record = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let dir = state_dir().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "sin directorio de estado")
            })?;
            let mut store = TrustStore::open(dir.join("lua-trust.toml"))?;
            store.record(&rec_path, &rec_bytes, allow)
        })
        .await;
        match record {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "err-lua-load",
                    &[("layer", "project"), ("detail", &io_error_category(&e))],
                ));
            }
            // Un panic al persistir es un bug NUESTRO: que reviente visible
            // (mismo criterio que los demás spawn_blocking de este binario).
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
        if allow && let Some(host) = host {
            eval_lua_layer(app, host, &bytes, Layer::Project);
        }
    }
    app.open_next_pending();
}

/// Arranca el comando Lua `name` con el snapshot ACTUAL de panes como
/// `PaneCtx` (congelado: determinismo > frescura). El TUI no tiene
/// multi-selección todavía: `selection` = la entrada bajo el cursor (o
/// vacía) — documentado, mismo dato que `current`. Comando no registrado →
/// barra `err-lua-unknown` (no es error de keymap) y `None`.
fn start_lua_run(
    app: &mut App,
    host: &LuaHost,
    backend: &Backend,
    name: &str,
) -> Option<(CommandRun, CancellationToken)> {
    let pane = app.focused();
    let other = &app.panes[1 - app.focus()];
    let current = pane.selected().map(|e| e.path.clone());
    let ctx = PaneCtx {
        cwd: pane.dir().clone(),
        other_cwd: other.dir().clone(),
        selection: current.clone().into_iter().collect(),
        current,
    };
    let token = CancellationToken::new();
    let Some(run) = host.invoke(name, backend.clone(), ctx, token.clone()) else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return None;
    };
    Some((run, token))
}

/// Despacha un binding `lua:<nombre>`: con un run en vuelo lo ENCOLA (FIFO,
/// tope [`LUA_QUEUE_MAX`]; llena = solo el aviso); libre, arranca. Sin host
/// (mlua no arrancó) el comando no puede existir → `err-lua-unknown`.
fn run_lua_command(
    app: &mut App,
    lua_host: Option<&LuaHost>,
    backend: &Backend,
    name: &str,
    lua_run: &mut Option<(CommandRun, CancellationToken)>,
    lua_queue: &mut VecDeque<String>,
) {
    let Some(host) = lua_host else {
        app.message = Some(ta("err-lua-unknown", &[("name", &detail_for_bar(name))]));
        return;
    };
    if lua_run.is_some() {
        if lua_queue.len() < LUA_QUEUE_MAX {
            lua_queue.push_back(name.to_owned());
            app.message = Some(t("msg-lua-busy"));
        } else {
            // Cola llena = DESCARTE: decirlo («encolado» mentiría).
            app.message = Some(t("msg-lua-queue-full"));
        }
        return;
    }
    *lua_run = start_lua_run(app, host, backend, name);
}

/// Recalcula la barra Lua (hook `norte.ui.statusbar`) con el snapshot del
/// pane con foco. El host cachea por `PartialEq` y CONGELA con un run en
/// vuelo (ver `lua::api`); su salida ya viene saneada. Un fallo del hook
/// (take-once) sale una vez por la barra y el hook queda deshabilitado
/// hasta el próximo hot-reload.
fn refresh_lua_status(app: &mut App, lua_host: Option<&LuaHost>) {
    let Some(host) = lua_host else {
        app.lua_status = None;
        return;
    };
    let pane = app.focused();
    let input = StatusInput {
        cwd: pane.dir().to_wire().into_bytes(),
        selected: pane.cursor(),
        // Sin multi-selección: los bytes de la entrada bajo el cursor.
        selected_bytes: pane.selected().and_then(|e| e.size).unwrap_or(0),
        entries: pane.entries().len(),
        tasks: app
            .board
            .rows()
            .iter()
            .filter(|r| !r.last.state.is_terminal())
            .count(),
    };
    app.lua_status = host.statusbar(&input);
    if let Some(detail) = host.statusbar_error() {
        app.message = Some(ta(
            "err-lua-statusbar",
            &[("detail", &detail_for_bar(&detail))],
        ));
    }
}

/// Tick: refresca snapshots del panel y reacciona a las tasks que ACABAN
/// de terminar — colisión con contexto → a la COLA de diálogos (jamás se
/// pisa un modal abierto, hallazgo B1); el resto → mensaje por categoría +
/// refresh de ambos panes (una mutación pudo cambiarlos).
/// (Strings de mensaje hardcodeados hasta Fluent — fase 9, issue #1.)
/// Devuelve `true` si ha REFRESCADO los panes (una mutación terminó): el run
/// loop suelta entonces cualquier relleno paginado en curso — `refresh_panes`
/// reescribe AMBOS panes con el listado completo, así que un drenador viejo
/// duplicaría entradas si siguiera vivo.
async fn on_tick(app: &mut App, backend: &Backend, events: &mut EventStream) -> bool {
    let finished = app.board.tick();
    if finished.is_empty() {
        app.open_next_pending();
        return false;
    }
    let mut refresh = false;
    for fin in finished {
        use norte_proto::TaskState;
        match fin.state {
            TaskState::Completed => {
                refresh = true;
                app.message = Some(t("msg-done"));
            }
            TaskState::Cancelled => {
                refresh = true;
                app.message = Some(t("msg-cancelled"));
            }
            TaskState::Failed { error } => {
                if let (Error::Unsupported, Some(target)) = (&error, &fin.trash_target) {
                    // La papelera no pudo AQUÍ (mount sin topdir…): se
                    // reofrece PERMANENTE con aviso — degradación con
                    // usuario informado (ADR 0009), jamás pisando un modal.
                    if app.modal.is_none() {
                        app.modal = Some(Modal::ConfirmDelete {
                            target: target.clone(),
                            permanent: true,
                        });
                    } else {
                        app.message = Some(t("msg-no-trash-here"));
                    }
                } else if let (Error::Conflict { .. }, Some(retry)) = (&error, fin.retry) {
                    app.pending_collisions.push_back(retry);
                } else {
                    // Render por CATEGORÍA localizado (spec §17.7, #20):
                    // jamás el Display inglés ni strings del OS.
                    app.message = Some(error_message(&error));
                    refresh = true;
                }
            }
            _ => {}
        }
    }
    app.open_next_pending();
    if refresh {
        refresh_panes(app, backend, events).await;
    }
    refresh
}

/// Recarga ambos panes tras una mutación (pueden mostrar el mismo dir).
/// CANCELABLE como el cd (regla 3): Esc abandona el refresh (los panes se
/// quedan como estaban), Ctrl-C sale. El cursor se conserva por ÍNDICE
/// (tras un delete queda en la siguiente entrada — semántica ortodoxa).
async fn refresh_panes(app: &mut App, backend: &Backend, events: &mut EventStream) {
    for i in 0..app.panes.len() {
        // Un pane en modo virtual de búsqueda (liveSearch T6) NO se
        // auto-refresca: `refresh_listing` lo sacaría del modo virtual y el
        // `reap` cancelaría la Task sin que el usuario saliera (review
        // MINOR-1). Sus hits viven fuera del FS: no hay dir real que recargar.
        if app.panes[i].virtual_search {
            continue;
        }
        let dir = app.panes[i].dir().clone();
        let fut = listing(backend, &dir);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                res = &mut fut => {
                    match res {
                        // El listado es COMPLETO: si venía de un cd paginado a
                        // medio rellenar, ya no está cargando (el run loop
                        // suelta el drenador tras este refresh). Un quick
                        // search vivo se re-aplica dentro (índices nuevos).
                        Ok((entries, skipped)) => {
                            app.panes[i].refresh_listing(entries);
                            // #96: el refresh trae las omitidas FRESCAS — sin
                            // esto, el badge conservaba el valor del listado
                            // anterior (rancio) tras una mutación.
                            app.panes[i].set_skipped(skipped);
                        }
                        // Sin silencio: el dir pudo desaparecer (issue #20).
                        Err(e) => app.message = Some(ta("msg-refresh-error", &[("error", &error_category(&e))])),
                    }
                    break;
                }
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(Event::Key(key)))
                            if key.kind == crossterm::event::KeyEventKind::Press =>
                        {
                            match (key.code, key.modifiers) {
                                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                    app.quit = true;
                                    return;
                                }
                                (KeyCode::Esc, _) => return,
                                _ => {}
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => return,
                    }
                }
            }
        }
    }
}

/// Teclas de un modal abierto, resueltas contra el contexto `dialog` del
/// keymap (H1 T2, issue #24 CERRADO — rebindeable) y filtradas por el
/// ALLOWLIST del modal concreto ([`dialog_action`]): la semántica de
/// seguridad vive en código, solo la ASIGNACIÓN tecla→comando es keymap.
/// `Modal::TrustLuaInit` nunca llega aquí (interceptado antes en el run
/// loop, decisión 8). `events` es para el reintento de navegación del modal
/// TOFU (#45): confiar en la host key relanza el `cd`, que tiene su propio
/// loop de eventos.
async fn on_dialog_key(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    let Some(modal) = app.modal.clone() else {
        return Cd::Cancelled;
    };
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run(cmd) => cmd,
        // Sin semántica de secuencia definida para overlays (T2): ignorar y
        // reiniciar el estado de resolución.
        Resolution::Pending(_) => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    let Some(outcome) = dialog_action(&modal, &cmd) else {
        return Cd::Cancelled; // comando fuera del allowlist de ESTE modal
    };
    match outcome {
        DialogOutcome::Open => {} // dialog_action nunca lo devuelve: defensivo
        DialogOutcome::Cancelled => {
            app.modal = None;
            app.open_next_pending();
            // Cerrar el diálogo de aprobación ES denegar (fail-safe): el
            // agente recibe `not-approved`, jamás una espera colgada.
            if let Modal::ApproveAgentOp { req } = modal {
                decide_approval(app, backend, req.approval_id, false).await;
            }
        }
        DialogOutcome::Confirmed => {
            app.modal = None;
            // OJO (MAJOR del rust-reviewer): NO abrir la siguiente pendiente
            // ANTES del match — el retry TOFU (`return cd`) puede reabrir un
            // TrustHostKey y PISAR una aprobación de agente ya sacada de la
            // cola (quedaría huérfana hasta su TTL). Se difiere al final.
            match modal {
                Modal::ConfirmDelete { target, permanent } => {
                    let del_mode = if permanent {
                        DeleteMode::Permanent
                    } else {
                        DeleteMode::Trash
                    };
                    match backend.delete(&target, del_mode).await {
                        Ok(task) => {
                            app.board
                                .push_full(task, None, (!permanent).then(|| target.clone()));
                        }
                        Err(e) => app.message = Some(error_message(&e)),
                    }
                }
                Modal::ConfirmTransfer { kind, from, to } => {
                    submit_transfer(app, backend, kind, from, to, TransferOptions::default()).await;
                }
                // TrustLuaInit se intercepta ANTES en el run loop (necesita
                // el LuaHost); MarkPattern (#103 T9) también, como texto
                // libre (mismo motivo que la búsqueda) — `dialog_action`
                // devuelve `None` para ambos, así que `on_dialog_key` ya
                // habría retornado antes de llegar a este match: inalcanzable
                // aquí, no-op defensivo.
                Modal::Collision { .. }
                | Modal::TrustLuaInit { .. }
                | Modal::MarkPattern { .. } => {}
                // S2 (`[ui] confirm_quit`): confirmar cierra — el run loop
                // lo detecta en su chequeo de `app.quit` de cada vuelta
                // (main.rs, tope del `loop`).
                Modal::ConfirmQuit => app.quit = true,
                Modal::ApproveAgentOp { req } => {
                    decide_approval(app, backend, req.approval_id, true).await;
                }
                // TOFU (#45): confía en la host key y REINTENTA la navegación.
                Modal::TrustHostKey {
                    host,
                    port,
                    algo,
                    fingerprint,
                    dir,
                } => {
                    match backend
                        .trust_host_key(&host, port, &algo, &fingerprint)
                        .await
                    {
                        Ok(()) => {
                            // El engine re-verifica el fingerprint contra la
                            // clave que el host presenta AHORA (anti-TOCTOU,
                            // ADR 0015 D); si aún falla, el retry lo mostrará.
                            let outcome = cd(app, backend, events, dir).await;
                            // Solo abrir la siguiente pendiente si el retry NO
                            // dejó un modal (otro HostKeyUnknown): jamás pisar.
                            if app.modal.is_none() {
                                app.open_next_pending();
                            }
                            return outcome;
                        }
                        Err(e) => app.message = Some(error_message(&e)),
                    }
                }
            }
            // Todas las ramas salvo el retry TOFU (que ya volvió) abren aquí
            // la siguiente pendiente, con el modal ya cerrado.
            app.open_next_pending();
        }
        DialogOutcome::Retry(policy) => {
            app.modal = None;
            if let Modal::Collision { retry } = modal {
                // Conserva las opciones ORIGINALES; solo cambia la política.
                let opts = TransferOptions {
                    on_collision: policy,
                    ..retry.opts
                };
                submit_transfer(app, backend, retry.kind, retry.from, retry.to, opts).await;
            }
            app.open_next_pending();
        }
    }
    // Salvo el retry TOFU (que hace `return cd(...)`), un modal no navega.
    Cd::Cancelled
}

/// Resuelve una aprobación de policy (`policy.decide`, M3-3b T5). Un error
/// (id ya vencido/decidido por otro frontend, daemon caído) sale por la
/// barra: la pendiente, si sigue viva, vencerá por TTL — jamás se cuelga.
async fn decide_approval(app: &mut App, backend: &Backend, approval_id: u64, approve: bool) {
    if let Err(e) = backend.policy_decide(approval_id, approve).await {
        app.message = Some(error_message(&e));
    }
}

/// Encola una transferencia y la registra en el panel con su contexto de
/// reintento (para el diálogo de colisión).
async fn submit_transfer(
    app: &mut App,
    backend: &Backend,
    kind: TransferKind,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
) {
    let res = match kind {
        TransferKind::Copy => backend.copy(&from, &to, opts).await,
        TransferKind::Move => backend.move_(&from, &to, opts).await,
    };
    match res {
        Ok(task) => {
            // #98/M1: el enc del pane origen viaja con el retry — la
            // colisión llega async y el foco puede haber cambiado.
            let name_encoding = app.focused().name_encoding();
            app.board.push(
                task,
                Some(RetrySpec {
                    kind,
                    from,
                    to,
                    opts,
                    name_encoding,
                }),
            );
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Traduce una tecla del diálogo de búsqueda (`Alt+F7`, liveSearch T6) a un
/// efecto sobre `App::search_dialog`. Teclas fijas como los demás overlays
/// (#24); `ctrl+c` conserva su salida global. Devuelve `Some(params)` SOLO
/// cuando Enter con algún criterio no vacío debe LANZAR la búsqueda (el caller
/// cierra el diálogo y abre el pane virtual); Enter sin criterio avisa y sigue.
fn on_search_dialog_key(
    app: &mut App,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<FsSearchParams> {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    let dialog = app.search_dialog.as_mut()?;
    // SHIFT pasa (mayúsculas/símbolos llegan como Char+SHIFT); ctrl/alt no
    // escriben en los campos.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    match code {
        // Toggles/Tab exigen `plain` (sin ctrl/alt): un Ctrl+F2 no togglea
        // (review MINOR-4), igual que el resto de la captura del diálogo.
        KeyCode::F(2) if plain => dialog.toggle_regex(),
        KeyCode::F(3) if plain => dialog.toggle_case(),
        KeyCode::Tab if plain => dialog.toggle_field(),
        KeyCode::Char(c) if plain => dialog.push_char(c),
        KeyCode::Backspace if plain => dialog.backspace(),
        KeyCode::Esc => app.search_dialog = None,
        KeyCode::Enter => {
            if dialog.has_criteria() {
                let root = app.focused().dir().clone();
                return Some(search_params(app.search_dialog.as_ref()?, root));
            }
            // Ambos campos vacíos: no-op con aviso (una búsqueda sin criterio
            // no tiene sentido). El diálogo sigue abierto.
            app.message = Some(t("search-empty"));
        }
        _ => {}
    }
    None
}

/// Construye los [`FsSearchParams`] del diálogo: el toggle `regex` decide, por
/// eje, `name_glob` vs `name_regex` y `content` vs `content_regex`; un campo
/// vacío no aporta criterio. `max_hits` se fija al tope por defecto
/// ([`SEARCH_MAX_HITS`]) — el diálogo v1 no lo expone.
fn search_params(dialog: &SearchDialog, root: VPath) -> FsSearchParams {
    let (name_glob, name_regex) = match (dialog.name.is_empty(), dialog.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(dialog.name.clone()), None),
        (false, true) => (None, Some(dialog.name.clone())),
    };
    let (content, content_regex) = match (dialog.content.is_empty(), dialog.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(dialog.content.clone()), None),
        (false, true) => (None, Some(dialog.content.clone())),
    };
    FsSearchParams {
        root,
        name_glob,
        name_regex,
        content,
        content_regex,
        case_sensitive: dialog.case,
        max_hits: Some(SEARCH_MAX_HITS),
    }
}

/// Lanza la búsqueda: `backend.search` → Err deja el diálogo abierto y avisa
/// por la barra (`search-status-failed`); Ok cierra el diálogo, guarda el dir
/// anterior, arranca el pane virtual y registra el [`SearchRun`] (cancelando
/// uno previo — el pane virtual es uno).
async fn launch_search(
    app: &mut App,
    backend: &Backend,
    fill: &mut Option<Fill>,
    search_run: &mut Option<SearchRun>,
    params: FsSearchParams,
) {
    let pane = app.focus();
    let root = params.root.clone();
    match backend.search(params).await {
        Ok((task, rx)) => {
            let prev_dir = app.panes[pane].dir().clone();
            app.search_dialog = None;
            app.message = None;
            app.panes[pane].begin_search(root);
            // El pane pasa a virtual: un relleno paginado en vuelo de ESTE
            // pane (dir aún cargándose) alimentaría el listado real como hits
            // (review MAJOR T6) — se suelta ya (tirante; `apply_fill_msg` es
            // el cinturón por si llega un lote antes).
            if fill.as_ref().is_some_and(|f| f.pane == pane) {
                *fill = None;
            }
            // Un run previo (raro: el diálogo se cierra al lanzar) se cancela.
            if let Some(old) = search_run.replace(SearchRun {
                task,
                rx,
                pane,
                prev_dir,
                hits: 0,
                state: SearchState::Running,
            }) {
                old.task.cancel();
            }
        }
        // Criterios inválidos u otro fallo del daemon/engine: el diálogo
        // SIGUE abierto (el usuario corrige) y el detalle va saneado a la
        // barra (categoría del error — jamás el patrón crudo).
        Err(e) => {
            app.message = Some(ta(
                "search-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Aplica un lote de hits (o el cierre del canal) al pane virtual. Un lote
/// llega mientras el pane siga en modo virtual; si un `cd` lo apagó, se suelta
/// el run (su drenador alimentaría un listado real) cancelando la Task.
/// `None` = fin del stream: se lee el estado terminal y se refleja en el pane.
fn drain_search(app: &mut App, search_run: &mut Option<SearchRun>, hits: Option<SearchHits>) {
    let Some(s) = search_run else {
        return;
    };
    if let Some(batch) = hits {
        if app.panes[s.pane].virtual_search {
            // #81: el contexto del match (línea + preview, saneado EN ORIGEN
            // por el core) se guarda por path — la barra lo pinta para el
            // hit bajo el cursor. Vista del pane sigue plana (v1).
            if let Some(infos) = batch.matches {
                // Contrato del wire: alineado 1:1. Un server bug que mande
                // menos matches truncaría el zip EN SILENCIO — ruido en dev.
                debug_assert_eq!(batch.entries.len(), infos.len(), "matches desalineados");
                for (e, info) in batch.entries.iter().zip(infos) {
                    app.panes[s.pane]
                        .search_matches
                        .insert(e.path.clone(), info);
                }
            }
            let n = batch.entries.len();
            app.panes[s.pane].extend_listing(batch.entries);
            s.hits += n;
        } else {
            s.task.cancel();
            *search_run = None;
        }
    } else {
        // Canal cerrado: el walker terminó. Estado terminal no bloqueante.
        let state = finalize_search_state(s);
        s.state = state;
        app.panes[s.pane].search_state = state;
        // El detalle concreto va a la barra UNA vez (error_message); el pane
        // guarda la CATEGORÍA para pintar `search-status-failed` de forma
        // persistente tras limpiarse el mensaje (review MINOR-2).
        if state == SearchState::Failed {
            let mut rx = s.task.progress();
            if let norte_proto::TaskState::Failed { error } = rx.borrow_and_update().state.clone() {
                app.panes[s.pane].search_error = Some(error_category(&error));
                app.message = Some(error_message(&error));
            }
        }
    }
}

/// Lee el estado terminal de un [`SearchRun`] del `TaskProgress` (no
/// bloqueante) y lo mapea a [`SearchState`]. `Completed` con los hits al tope
/// = `Truncated`; sin tope = `Done`. Un canal cerrado sin estado terminal aún
/// publicado (carrera) se trata como `Done` (el walker ya no emite).
fn finalize_search_state(s: &SearchRun) -> SearchState {
    let mut rx = s.task.progress();
    match rx.borrow_and_update().state.clone() {
        norte_proto::TaskState::Cancelled => SearchState::Cancelled,
        norte_proto::TaskState::Failed { .. } => SearchState::Failed,
        norte_proto::TaskState::Completed if s.hits >= SEARCH_MAX_HITS as usize => {
            SearchState::Truncated
        }
        _ => SearchState::Done,
    }
}

/// Esc en el pane virtual de búsqueda (liveSearch T6): con la Task viva pide
/// cancelación (los hits ya llegados se conservan; el estado pasará a
/// `Cancelled` al cerrarse el canal); ya terminada, sale del modo virtual
/// restaurando el dir anterior con un `cd` normal.
async fn on_search_escape(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut Option<Fill>,
    last_probed: &mut Option<(usize, VPath)>,
    search_run: &mut Option<SearchRun>,
) {
    let Some(s) = search_run.as_ref() else {
        return;
    };
    if s.state == SearchState::Running {
        s.task.cancel();
        return;
    }
    let prev = s.prev_dir.clone();
    *search_run = None;
    let outcome = cd(app, backend, events, prev).await;
    apply_cd(fill, last_probed, outcome);
}

/// Enter sobre un hit del pane virtual (liveSearch T6): cd al PADRE del hit y
/// deja el cursor sobre él (por path, si ya está en la primera página).
/// Cancela la Task si sigue viva y sale del modo virtual.
async fn on_search_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    fill: &mut Option<Fill>,
    last_probed: &mut Option<(usize, VPath)>,
    search_run: &mut Option<SearchRun>,
) {
    let Some(hit) = app.focused().selected().map(|e| e.path.clone()) else {
        return;
    };
    let Some(parent) = hit.parent() else {
        return;
    };
    if let Some(s) = search_run.as_ref()
        && s.state == SearchState::Running
    {
        s.task.cancel();
    }
    *search_run = None;
    let pane = app.focus();
    let outcome = cd(app, backend, events, parent).await;
    apply_cd(fill, last_probed, outcome);
    // Re-ancla el cursor sobre el hit por path (el cd resetea a 0); si cayó
    // en una página aún no drenada, el cursor se queda arriba (v1).
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
}

/// Suelta el [`SearchRun`] si su pane SALIÓ del modo virtual (un `cd`/refresh
/// lo apagó): su drenador alimentaría un listado real. Cancela la Task si
/// sigue viva (regla 3).
fn reap_search_run(app: &App, search_run: &mut Option<SearchRun>) {
    if let Some(s) = search_run.as_ref()
        && !app.panes[s.pane].virtual_search
    {
        s.task.cancel();
        *search_run = None;
    }
}

/// Resuelve el opener (#28) del fichero seleccionado y, si TODO valida, deja
/// el `(programa, argv)` en `app.pending_open` para que el run loop lo lance
/// (es dueño de la terminal). Cada fallo va a la barra — degradación limpia,
/// jamás un lanzamiento a ciegas: sin fichero (no-op), remoto/archivo
/// (`msg-open-remote`), sin opener para el mime (`msg-open-no-opener`), o
/// binario ausente (`msg-open-missing-program`).
fn resolve_opener(app: &mut App) {
    use norte_frontend::openers;
    let Some(path) = app
        .focused()
        .selected()
        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
        .map(|e| e.path.clone())
    else {
        return;
    };
    // Ruta nativa: SOLO `file://` local; archive/sftp/s3 → sin path nativo.
    let Ok(native) = norte_vfs_local::vpath_to_native(&path) else {
        app.message = Some(t("msg-open-remote"));
        return;
    };
    let mime = openers::guess_mime(
        path.file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes),
    );
    let Some(opener) = app.openers.resolve(mime) else {
        app.message = Some(ta("msg-open-no-opener", &[("mime", mime)]));
        return;
    };
    let program = opener.program().to_owned();
    // La sonda del binario en el PATH (`program_available`) es I/O de disco:
    // NO se hace aquí (camino async) — el run loop la corre en spawn_blocking
    // junto al lanzamiento (regla 2).
    // `%d` = el directorio del pane (nativo); si por lo que sea no convierte,
    // el padre del propio fichero.
    let dir = norte_vfs_local::vpath_to_native(app.focused().dir()).unwrap_or_else(|_| {
        native
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    });
    app.pending_open = Some((program, opener.argv(&[&native], &dir)));
}

/// Sondea el binario del opener en el PATH (#28) — I/O de disco en
/// `spawn_blocking`, JAMÁS en el executor async (regla 2) — y, si existe,
/// suspende el TUI y lo lanza. Devuelve el mensaje de barra LOCALIZADO del
/// resultado (binario ausente / lanzado / fallo de spawn).
async fn launch_opener(
    terminal: &mut ratatui::DefaultTerminal,
    program: String,
    argv: Vec<std::ffi::OsString>,
) -> String {
    let prog = program.clone();
    let available =
        tokio::task::spawn_blocking(move || norte_frontend::openers::program_available(&prog))
            .await
            .unwrap_or(false);
    if !available {
        return ta("msg-open-missing-program", &[("program", &program)]);
    }
    match run_opener(terminal, argv).await {
        Ok(_) => ta("msg-open-launched", &[("program", &program)]),
        Err(e) => ta(
            "msg-open-failed",
            &[("program", &program), ("error", &e.to_string())],
        ),
    }
}

/// Suspende el TUI (sale de la pantalla alternativa + raw mode), lanza el
/// comando externo con stdio HEREDADO (el usuario interactúa con `bat`/editor
/// directamente) y espera su fin en `spawn_blocking` (regla 2). La terminal se
/// restaura SIEMPRE una vez que se dejó el modo TUI — falle el hijo, se rompa
/// el join o falle una syscall intermedia — para no dejarla en raw-off. #28.
async fn run_opener(
    terminal: &mut ratatui::DefaultTerminal,
    argv: Vec<std::ffi::OsString>,
) -> std::io::Result<std::process::ExitStatus> {
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    // Invariante: `Opener::argv` siempre empuja el binario (`command[0]`), y
    // `parse` rechaza `command` vacío — así `argv[0]` nunca panica.
    debug_assert!(
        !argv.is_empty(),
        "el argv de un opener siempre trae el binario"
    );
    disable_raw_mode()?;
    // A partir de aquí la terminal está fuera del modo TUI: restaurar pase lo
    // que pase antes de devolver.
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;
    let child = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
    })
    .await;
    let mut restore = || -> std::io::Result<()> {
        crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
        enable_raw_mode()?;
        terminal.clear()
    };
    let restored = restore();
    // El resultado del hijo prima; si además falló la restauración, se propaga.
    let status = child.map_err(std::io::Error::other)?;
    restored?;
    status
}

/// Ejecuta un comando nombrado (ADR 0006: los mismos nombres que verán la
/// palette y el wire). Un error de listado en un cd NO tumba el TUI: el
/// pane se queda donde estaba (aviso visible: barra de mensajes, issue #20).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // tabla de despacho comando→efecto, no API
async fn dispatch(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    help_lines: &[String],
    quick_mode: nav::Mode,
    confirm_quit: config::ConfirmQuit,
    // S3 (`app.settings`): la config VIGENTE — solo leída, para construir
    // las filas del overlay al abrirlo (`crate::settings::build_rows`).
    cfg: &config::LoadedConfig,
    cmd: &str,
) -> Cd {
    // Solo los cd (nav.enter/nav.parent) tocan el relleno en background; el
    // resto de comandos lo dejan como está (`Cancelled`).
    let mut cd_outcome = Cd::Cancelled;
    match cmd {
        // S2 (`[ui] confirm_quit`): SOLO este brazo (el despacho nombrado de
        // `app.quit`, alcanzable por keymap Y por la palette) honra la
        // config y puede abrir `Modal::ConfirmQuit`. Los `app.quit = true`
        // hardcodeados de Ctrl+C repartidos por el resto de este fichero
        // (cada overlay tiene el suyo, documentado in situ) son la salida de
        // emergencia — se quedan INMEDIATOS a propósito, jamás preguntan.
        "app.quit" => {
            if norte_tui::app::quit_needs_confirm(confirm_quit, app.board.has_active()) {
                app.modal = Some(Modal::ConfirmQuit);
            } else {
                app.quit = true;
            }
        }
        "pane.switch" => app.switch_focus(),
        // `/` (spec 2026-07-18): arranca el quick search en el modo de la
        // config. Con uno ya activo las teclas se comen antes del resolver,
        // así que este brazo solo corre para ABRIRLO — sin recursión.
        "pane.quick-search" => app.focused_mut().quick_start(quick_mode),
        // `Alt+↓` / `Ctrl+D` (spec 2026-07-18): con el popup abierto sus
        // teclas se comen antes del resolver (patrón overlay) — estos
        // brazos solo corren para ABRIRLO.
        "pane.history" => app.open_nav_popup(NavPopupKind::History),
        "pane.hotlist" => app.open_nav_popup(NavPopupKind::Hotlist),
        // `Alt+F7` (liveSearch T6): abre el diálogo de búsqueda viva. Con él
        // abierto sus teclas se comen antes del resolver (patrón overlay) —
        // este brazo solo corre para ABRIRLO.
        "pane.search" => app.open_search_dialog(),
        "cursor.up" => app.focused_mut().move_up(1),
        "cursor.down" => app.focused_mut().move_down(1),
        "cursor.page-up" => app.focused_mut().move_up(PAGE),
        "cursor.page-down" => app.focused_mut().move_down(PAGE),
        "cursor.top" => app.focused_mut().move_to_start(),
        "cursor.bottom" => app.focused_mut().move_to_end(),
        "nav.enter" => {
            // También symlinks: si apunta a un dir, el provider listará; si
            // no, el cd falla y se absorbe — qué es "entrable" lo decide el
            // core, no el TUI (regla 7). Un File .zip/.tar entra como
            // directorio virtual (ADR 0018): el TUI solo COMPONE el path
            // (azúcar de navegación); listar/validar sigue siendo del core.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Symlink))
                .map(|e| e.path.clone())
                .or_else(|| app.focused().selected().and_then(archive_root_for));
            if let Some(dir) = target {
                cd_outcome = cd(app, backend, events, dir).await;
            }
        }
        "nav.parent" => {
            // Salir de la raíz interior de un archivo = el dir que CONTIENE
            // al contenedor (el padre sintáctico sería un compuesto sin
            // marcador: malformado, ADR 0018).
            let dir = app.focused().dir().clone();
            // Foco pendiente (spec 2026-07-24 §S1): el hijo del que
            // venimos, para seleccionarlo en el listado del padre. Al salir
            // de la raíz interior de un archivo el hijo NO es `dir` (ese es
            // el path compuesto virtual, no una entrada real del listado
            // del padre) sino el archivo contenedor mismo (`aref.outer`).
            let (parent, child) = match dir.archive_split() {
                Ok(Some(aref)) if aref.inner.is_empty() => {
                    let outer = aref.outer.clone();
                    (outer.parent(), outer)
                }
                _ => (dir.parent(), dir.clone()),
            };
            if let Some(parent) = parent {
                app.focused_mut().set_pending_focus(child);
                cd_outcome = cd(app, backend, events, parent).await;
                // Revisión S, M2: un `cd` FALLIDO (permiso denegado, error
                // del daemon…) nunca llama a `set_listing` (`cd`'s doc, `Err`
                // arm), así que el hint recién fijado arriba nunca se
                // consume — descartarlo aquí evita que sobreviva a un `cd`
                // futuro sin relación. `Cd::Cancelled` (p. ej. el modal TOFU,
                // que REINTENTA esta misma navegación) lo CONSERVA a
                // propósito: el reintento debe seguir aterrizando en `child`.
                if matches!(cd_outcome, Cd::Failed(_)) {
                    app.focused_mut().clear_pending_focus();
                }
            } else {
                // Raíz `/` o raíz de unidad Windows (`parent()` = None): antes
                // era un no-op SILENCIOSO (#20). Ahora avisa por la barra.
                app.message = Some(t("msg-nav-at-top"));
            }
        }
        "pane.copy" | "pane.move" => {
            let kind = if cmd == "pane.copy" {
                TransferKind::Copy
            } else {
                TransferKind::Move
            };
            // Destino ortodoxo: el dir del OTRO pane + el mismo nombre.
            let other = &app.panes[1 - app.focus()];
            let target = app.focused().selected().and_then(|e| {
                let name = e.path.file_name()?.clone();
                Some((e.path.clone(), other.dir().join(name)))
            });
            if let Some((from, to)) = target {
                app.modal = Some(Modal::ConfirmTransfer { kind, from, to });
            }
        }
        // Insert/Ctrl+A/Ctrl+Shift+A/`*` (#103): mc/Total Commander —
        // togglear la marca de esta entrada y avanzar (mantener Insert barre
        // un rango). Review MAJOR: bajo un quick search en Filter,
        // `toggle_mark` actúa sobre la selección FILTRADA mientras el cursor
        // real es otra cosa — avanzar el cursor real desincroniza el rango
        // barrido del filtro. La composición completa (marcar + a qué avanza
        // según haya o no filtro, clampado sin envolver) vive en el modelo
        // compartido.
        "mark.toggle" => app.focused_mut().toggle_mark_and_advance(),
        "mark.all" => app.focused_mut().mark_all(),
        "mark.invert" => app.focused_mut().invert_marks(),
        "mark.clear" => app.focused_mut().clear_marks(),
        // `+`/`-` (#103 T9): abren el modal de patrón (texto libre, ver el
        // brazo `app.modal.is_some()` de arriba) — marcar/desmarcar
        // corre al confirmar (`mark_pattern_confirm`), no aquí.
        "mark.pattern-add" => app.open_mark_pattern(true),
        "mark.pattern-remove" => app.open_mark_pattern(false),
        "pane.delete" | "pane.delete-permanent" => {
            if let Some(e) = app.focused().selected() {
                // F8 = papelera si el provider la declara; sin ella, el
                // MISMO diálogo avisa de PERMANENTE (degradación con
                // usuario informado, ADR 0009). shift+f8 = permanente.
                let hay_papelera = backend
                    .capabilities(&e.path)
                    .await
                    .is_ok_and(|c| c.flags.contains(norte_proto::CapabilityFlags::TRASH));
                let permanent = cmd == "pane.delete-permanent" || !hay_papelera;
                app.modal = Some(Modal::ConfirmDelete {
                    target: e.path.clone(),
                    permanent,
                });
            }
        }
        "pane.view" => {
            // También symlinks (mismo criterio que nav.enter): si apunta a
            // un dir, el read fallará con mensaje visible.
            let target = app
                .focused()
                .selected()
                .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
                .map(|e| e.path.clone());
            if let Some(path) = target {
                open_viewer(app, backend, events, path).await;
            }
        }
        "pane.open" => resolve_opener(app),
        "viewer.close" => app.viewer = None,
        "viewer.up" => viewer_do(app, |v| v.scroll_up(1)),
        "viewer.down" => viewer_do(app, |v| v.scroll_down(1)),
        "viewer.page-up" => viewer_do(app, |v| v.scroll_up(norte_tui::viewer::PAGE)),
        "viewer.page-down" => viewer_do(app, |v| v.scroll_down(norte_tui::viewer::PAGE)),
        "viewer.top" => viewer_do(app, norte_tui::viewer::Viewer::scroll_top),
        "viewer.bottom" => viewer_do(app, norte_tui::viewer::Viewer::scroll_bottom),
        "viewer.encoding" => viewer_do(app, norte_tui::viewer::Viewer::cycle_encoding),
        "viewer.encoding-auto" => viewer_do(app, norte_tui::viewer::Viewer::reset_encoding),
        "viewer.hex" => viewer_do(app, norte_tui::viewer::Viewer::toggle_hex),
        "app.help" => {
            app.help = Some(Help {
                lines: help_lines.to_vec(),
                scroll: 0,
            });
        }
        "pane.names-encoding" => {
            // #57: cicla la reinterpretación de nombres no-UTF8 del pane con
            // foco (display-only, regla 1). El anuncio va por la barra.
            let label = app.focused_mut().cycle_name_encoding();
            app.message = Some(match label {
                Some(enc) => ta("msg-names-encoding", &[("enc", enc)]),
                None => t("msg-names-encoding-off"),
            });
        }
        "app.theme" => app.open_theme_picker(),
        "app.extensions" => match backend.plugins_list().await {
            // El catálogo llega YA ordenado por categoría e id desde el core.
            Ok(list) => {
                // (P1 encoding audit F1) INGEST: clampa+enmascara `description`
                // UNA vez aquí, no en cada frame de `plugin_description_line`
                // — defensa contra un daemon hostil/comprometido que ignore
                // el tope del manifiesto.
                let mut plugins = list.plugins;
                norte_tui::app::clamp_plugin_descriptions(&mut plugins);
                app.extensions = Some(ExtensionManager {
                    plugins,
                    errors: list.errors,
                    cursor: 0,
                    config: None,
                });
            }
            Err(e) => app.message = Some(error_message(&e)),
        },
        // Ctrl+P / vim `:` (H1 T4, spec-promised): abre la palette sobre la
        // snapshot PRECOMPUTADA (`App::palette_rows`, `main::build_keymaps`
        // + hot-reload) — jamás recalcula el keymap efectivo aquí. Elegir
        // `app.palette` DESDE la palette (el run loop la cierra ANTES de
        // despachar, `enter`) es un no-op observable: cierra y reabre
        // vacía — inofensivo, sin recursión de estado.
        //
        // (P1) ahora es ASÍNCRONA, como `app.extensions` arriba: las filas
        // de plugin necesitan `backend.plugins_list().await` (aprobado +
        // activado, `palette::plugin_rows`). A diferencia de `app.extensions`
        // (que NO abre el gestor si el fetch falla), los built-ins SIEMPRE
        // deben poder despacharse — un daemon caído no debe tumbar la
        // palette entera, solo degradarla (sin filas de plugin + un aviso),
        // mismo principio "un error de listado no tumba el TUI" del resto
        // de `dispatch`.
        "app.palette" => {
            // MINOR-6 (H1 close): Ctrl+P/`:` viven en `[global]`, fundido en
            // AMBOS efectivos — la palette puede abrirse desde el viewer
            // también, no solo desde browse (`rows_for_context` doc).
            let mut rows =
                norte_tui::palette::rows_for_context(&app.palette_rows, app.viewer.is_some());
            match backend.plugins_list().await {
                Ok(list) => {
                    // (P1 encoding audit F1) INGEST: mismo clamp que el brazo
                    // `app.extensions` — un solo punto de entrada, mismo tope.
                    let mut plugins = list.plugins;
                    norte_tui::app::clamp_plugin_descriptions(&mut plugins);
                    rows.extend(norte_tui::palette::plugin_rows(&plugins));
                }
                Err(e) => app.message = Some(error_message(&e)),
            }
            app.palette = Some(Palette::new(rows));
        }
        // `F11` (S3): overlay de ajustes — las filas nacen del `cfg` VIGENTE
        // (mismo criterio que `help_lines`/`app.palette_rows`: reconstruidas
        // al abrir, jamás una copia arrastrada). Sección Plugins (G3c): un
        // resumen POR plugin con `[config]` (real ahora, ya no la nota
        // informativa de P2 — `plugin_config_summaries`).
        "app.settings" => {
            let summaries = plugin_config_summaries(backend).await;
            app.settings = Some(Settings::new(norte_tui::settings::build_rows(
                cfg, &summaries,
            )));
        }
        "task.cancel" => {
            app.message = Some(if app.board.cancel_last_running() {
                t("msg-cancelling")
            } else {
                t("msg-no-tasks")
            });
        }
        // (P1) Enter sobre una fila de plugin de la palette: `cmd` es la
        // `key` de despacho `plugin:{id}:{command}` (`palette::plugin_rows`,
        // JAMÁS pintada) — no vive en `COMMANDS`, así que necesita su propio
        // brazo ANTES del comodín de abajo. `parse_plugin_key` documenta por
        // qué el split es inequívoco pese a que `command` no tiene charset
        // validado. El resultado del plugin es texto NO confiable: por
        // `detail_for_bar` (enmascarado + tope, patrón #73) antes de la
        // barra de estado.
        _ if cmd.starts_with("plugin:") => {
            if let Some((id, command)) = parse_plugin_key(cmd) {
                app.message = Some(match backend.plugin_run_command(id, command, "").await {
                    Ok(output) => ta("msg-plugin-run-ok", &[("output", &detail_for_bar(&output))]),
                    Err(e) => error_message(&e),
                });
            }
        }
        // Inalcanzable: todo keymap se valida contra COMMANDS al cargar
        // (y COMMANDS vive en la lib: una sola fuente) — salvo el brazo de
        // plugin de arriba, que no vive en COMMANDS a propósito.
        _ => debug_assert!(false, "comando validado sin brazo: {cmd}"),
    }
    cd_outcome
}

/// #103 T9: `mark.pattern-add`/`-remove` llegaron a `COMMANDS` y a los tres
/// presets (`keymap.rs`) SIN un brazo en `dispatch` — el comodín final es
/// `debug_assert!(false, ...)`, así que pulsar `+`/`-` PANICABA un build
/// debug (silencioso en release, un no-op). El fix ideal invocaría
/// `dispatch` con cada nombre de `COMMANDS` y comprobaría que no cae al
/// comodín, pero eso resultó INALCANZABLE sin reescribir `dispatch`:
///
/// - Es `async fn` y pide un `&mut EventStream` REAL. `EventStream::new()`
///   arranca un hilo que llama a `poll_internal` de inmediato, y ESO panica
///   fuera de un terminal real (`"reader source not set"`, interno de
///   crossterm, sin gancho de test expuesto) — confirmado empíricamente al
///   intentarlo. Coincide con un límite YA señalado en este código
///   (`app.rs`, comentario junto a
///   `mark_toggle_advances_without_wrapping_at_the_end`: "`dispatch` en sí
///   no es testeable aquí sin un daemon real").
/// - No hay una estructura PURA aparte que decida los brazos: extraer una
///   tabla comando→acción sería una reescritura de `dispatch` (~270 líneas
///   de match) fuera de alcance de esta tarea.
///
/// Así que esto pinea la propiedad MÁS DÉBIL que SÍ es alcanzable sin
/// ejecutar ni reescribir `dispatch`: cada nombre de `COMMANDS` aparece como
/// LITERAL DE CADENA dentro del cuerpo fuente real de la función (extraído
/// de este mismo fichero por conteo de llaves desde la firma). Cubre
/// EXACTAMENTE la clase de bug que motivó esta tarea — un comando de
/// `COMMANDS` sin ningún string coincidente en el cuerpo, como
/// `mark.pattern-add` antes de este commit.
///
/// Lo que NO cubre, a propósito documentado: que el literal encontrado sea
/// código VIVO (uno que solo viviera dentro de un comentario colaría); que
/// el brazo sea semánticamente correcto (cosa de sus propios tests); ni que
/// el comando no caiga por accidente en el brazo de OTRO (un typo que
/// coincida con un string ajeno no se detecta). El conteo de llaves asume
/// que las llaves DENTRO de los literales de cadena del cuerpo están
/// balanceadas — cierto hoy (el único caso, el format string del propio
/// comodín `"...: {cmd}"`, es 1 abre + 1 cierra) pero no está garantizado
/// para siempre; un desbalance futuro haría panicar la extracción con un
/// mensaje claro, jamás pasar en silencio.
#[cfg(test)]
mod command_dispatch_tests {
    use super::COMMANDS;

    /// Cuerpo de `async fn dispatch(` de este mismo fichero, desde su `{`
    /// de apertura hasta el `}` que la cierra (conteo de llaves: sin tirar
    /// de `syn` para un solo test, regla 8 de CLAUDE.md — nueva dependencia
    /// solo se justifica con un uso real).
    fn dispatch_body() -> &'static str {
        const SRC: &str = include_str!("main.rs");
        let sig = SRC
            .find("async fn dispatch(")
            .expect("dispatch debe existir en este fichero");
        let open = SRC[sig..].find('{').expect("firma con cuerpo") + sig;
        let mut depth = 0i32;
        for (i, c) in SRC[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &SRC[open..=open + i];
                    }
                }
                _ => {}
            }
        }
        panic!("cuerpo de dispatch sin cierre (conteo de llaves desbalanceado)");
    }

    #[test]
    fn every_command_has_a_matching_string_literal_in_dispatch() {
        let body = dispatch_body();
        // Sanity: la extracción encontró el cuerpo REAL, no un prefijo
        // vacío o cortado en falso por algún string con llaves.
        assert!(
            body.contains("\"cursor.up\"") && body.contains("cd_outcome"),
            "extracción de dispatch_body sospechosa: {} bytes",
            body.len()
        );
        let missing: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .filter(|cmd| !body.contains(&format!("\"{cmd}\"")))
            .collect();
        assert!(
            missing.is_empty(),
            "COMMANDS sin literal coincidente en el cuerpo de dispatch — \
             pulsarlos hoy panica el comodín en debug (y es un no-op en \
             release): {missing:?}"
        );
    }
}

/// Parsea una `key` de fila de plugin de la palette
/// (`plugin:{plugin_id}:{command_id}`, [`norte_tui::palette::plugin_rows`])
/// de vuelta a `(plugin_id, command_id)`. El `plugin_id` es reverse-DNS
/// charset-validado por el core (`is_valid_plugin_id`, norte-plugin-host
/// manifest.rs — nunca lleva `:`); el `command_id` del manifiesto NO tiene
/// charset validado, así que puede llevar CUALQUIER byte, incluidos `:` o
/// saltos de línea. El PRIMER `:` que sigue al prefijo `plugin:` separa
/// ambos sin ambigüedad (el `plugin_id` no puede contenerlo) — el resto,
/// TODO lo que quede tras ese primer `:`, es el `command_id` crudo, tomado
/// ENTERO y jamás vuelto a partir.
fn parse_plugin_key(cmd: &str) -> Option<(&str, &str)> {
    let (id, command) = cmd.strip_prefix("plugin:")?.split_once(':')?;
    (!id.is_empty()).then_some((id, command))
}

/// Builds the Plugins-section summaries for the settings overlay (G3c):
/// `plugins_list` (approved+enabled only — same gate the palette's
/// `plugin_rows` and the extension manager's actionable rows use) then one
/// `plugin.get_config` PER surviving plugin, keeping only those with at
/// least one `[config.<key>]` (nothing to summarize/drill into otherwise).
/// Best-effort: a plugin whose `get_config` call fails (daemon hiccup, a
/// remote N-1 without the method) is simply DROPPED from the section — an
/// enrichment lost, never a hard error that would block opening settings
/// at all (same fallback contract as `plugin.decorate`/`column_values`).
/// `name` is masked here (plugin text, untrusted) — the ONLY point this
/// summary crosses into `norte_frontend::settings::Row`.
async fn plugin_config_summaries(
    backend: &Backend,
) -> Vec<norte_frontend::settings::PluginConfigSummary> {
    let Ok(list) = backend.plugins_list().await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in list.plugins.iter().filter(|p| p.approved && p.enabled) {
        let Ok(cfg) = backend.plugin_get_config(&p.id).await else {
            continue;
        };
        if cfg.keys.is_empty() {
            continue;
        }
        let (name, _) = norte_tui::app::display_name(p.name.as_bytes());
        out.push(norte_frontend::settings::PluginConfigSummary {
            plugin_id: p.id.clone(),
            name,
            key_count: cfg.keys.len(),
        });
    }
    out
}

#[cfg(test)]
mod parse_plugin_key_tests {
    use super::parse_plugin_key;

    #[test]
    fn separa_plugin_id_y_command_id() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:greet"),
            Some(("org.norte.demo", "greet"))
        );
    }

    /// El `command_id` NO tiene charset validado (a diferencia del
    /// `plugin_id`): puede llevar `:` o saltos de línea, y el split se
    /// queda con TODO lo que sigue al primero, sin volver a partir.
    #[test]
    fn command_id_hostil_se_toma_entero_sin_repartir() {
        assert_eq!(
            parse_plugin_key("plugin:org.norte.demo:a:b\nc"),
            Some(("org.norte.demo", "a:b\nc"))
        );
    }

    #[test]
    fn sin_prefijo_plugin_es_none() {
        assert_eq!(parse_plugin_key("app.quit"), None);
        assert_eq!(parse_plugin_key(""), None);
    }

    /// Sin el segundo `:` (formato mínimo `plugin:x` sin `command_id`): `None`
    /// — un despacho parcial jamás corre `plugin_run_command` con un id
    /// vacío o adivinado.
    #[test]
    fn sin_segundo_separador_es_none() {
        assert_eq!(parse_plugin_key("plugin:org.norte.demo"), None);
    }

    /// `plugin_id` vacío (`"plugin::greet"`) es `None` — nunca alcanzable
    /// desde una fila real (`PluginInfo.id` siempre no-vacío, validado por
    /// el core), pero el parser no debe entregar un id vacío a
    /// `plugin_run_command` si alguna vez lo fuera.
    #[test]
    fn plugin_id_vacio_es_none() {
        assert_eq!(parse_plugin_key("plugin::greet"), None);
    }
}

fn viewer_do(app: &mut App, f: impl FnOnce(&mut Viewer)) {
    if let Some(v) = &mut app.viewer {
        f(v);
    }
}

/// Presupuesto de lectura del viewer: cabecera de 256 KiB (el resto del
/// archivo NO se lee — rango de ADR 0005; «cargar más» = deuda de M2).
/// OJO si esto crece (>~1 MiB): `Viewer::recompute` y `rows()` corren en
/// el hilo del loop — harían falta `spawn_blocking` + índice de líneas.
const VIEW_CAP: u64 = 256 * 1024;

/// Abre el viewer leyendo la CABECERA vía el core (regla 7), cancelable
/// como el cd (Esc abandona, Ctrl-C sale).
async fn open_viewer(app: &mut App, backend: &Backend, events: &mut EventStream, path: VPath) {
    // Fase 1 leer la cabecera, fase 2 (M4-P5) intentar el preview de un plugin.
    // Ambas van dentro de la MISMA future para que Esc/Ctrl-C cancelen en
    // cualquiera de las dos. Un fallo del preview NUNCA impide ver el crudo.
    let fut = async {
        let (bytes, truncated) = read_head(backend, &path).await?;
        // G3a (ADR 0037): intenta el preview CON ESTILO primero; `Ok(None)`
        // (ningún previewer aplica, un guest cayó, o los topes del wire se
        // violaron — todos degradan igual, ver `Backend::
        // plugin_preview_styled`) cae al preview PLANO clásico, que a su
        // vez cae a la vista cruda si tampoco aplica. Un fallo de RED (no
        // `Ok`) en el intento estilizado tampoco bloquea: se trata igual
        // que `None` y se reintenta con el plano (mismo criterio que ya
        // regía para el plano frente a la vista cruda).
        let viewer = match backend.plugin_preview_styled(&path).await {
            Ok(Some(p)) => {
                Viewer::with_plugin_preview_styled(path.clone(), p.plugin_name, &p.lines, p.lossy)
            }
            Ok(None) | Err(_) => match backend.plugin_preview(&path).await {
                Ok(res) => match res.preview {
                    Some(p) => {
                        Viewer::with_plugin_preview(path.clone(), p.plugin_name, &p.output, p.lossy)
                    }
                    None => Viewer::new(path.clone(), bytes, truncated),
                },
                // Un plugin roto no bloquea el archivo: vista cruda de siempre.
                Err(_) => Viewer::new(path.clone(), bytes, truncated),
            },
        };
        Ok::<Viewer, Error>(viewer)
    };
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok(viewer) => app.viewer = Some(viewer),
                    Err(e) => app.message = Some(ta("msg-view-error", &[("error", &error_category(&e))])),
                }
                return;
            }
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                app.quit = true;
                                return;
                            }
                            (KeyCode::Esc, _) => return,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return,
                }
            }
        }
    }
}

/// Lee hasta `VIEW_CAP + 1` bytes: el byte extra delata el truncado.
async fn read_head(backend: &Backend, path: &VPath) -> Result<(Vec<u8>, bool), Error> {
    let mut out = backend
        .read(
            path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(VIEW_CAP + 1),
            }),
        )
        .await?;
    let truncated = out.len() as u64 > VIEW_CAP;
    if truncated {
        out.truncate(usize::try_from(VIEW_CAP).unwrap_or(usize::MAX));
    }
    Ok((out, truncated))
}

/// Si la entrada es un contenedor navegable (`.<formato>` de la whitelist
/// de proto, extensión ASCII case-insensitive), la raíz de su interior
/// (ADR 0018). El mapa extensión→formato es azúcar de presentación; la
/// validación real es del core. Un SYMLINK a un archivo no entra como
/// contenedor en v1 (decisión consciente: exigiría resolver el target por
/// stat del core; issue de fase 8g).
fn archive_root_for(e: &norte_proto::Entry) -> Option<VPath> {
    // Extensiones cuyo sufijo no coincide con el token del formato (#55):
    // `tar+gz` no tiene un `.tar+gz` real en el mundo, la gente escribe
    // `.tgz`/`.tar.gz`. Se comprueban ANTES del genérico `.{formato}` — un
    // `.tar.gz` no casaría de todos modos con `.tar` (termina en `.gz`), así
    // que el orden es defensivo, no estrictamente necesario hoy.
    const EXT_ALIASES: &[(&[u8], &str)] = &[(b".tar.gz", "tar+gz"), (b".tgz", "tar+gz")];
    fn ends_ci(name: &[u8], suffix: &[u8]) -> bool {
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    }
    if e.kind != EntryKind::File {
        return None;
    }
    let name = e.path.file_name()?.as_bytes();
    let format = EXT_ALIASES
        .iter()
        .find(|(suffix, _)| ends_ci(name, suffix))
        .map(|(_, format)| *format)
        .or_else(|| {
            norte_proto::ARCHIVE_FORMATS
                .iter()
                .find(|f| ends_ci(name, format!(".{f}").as_bytes()))
                .copied()
        })?;
    // Falla (exterior con `!`, ya compuesto…): no es navegable — Enter no-op.
    VPath::archive_compose(format, &e.path, &[]).ok()
}

/// Listado COMPLETO de `dir` (para `refresh_panes` tras una mutación:
/// conserva el cursor por índice). Una entrada con error corta el listado —
/// mejor un error honesto que un listado silenciosamente incompleto. #54: NO
/// ordena aquí — `refresh_listing`/`PaneState::refill` normalizan
/// internamente, un sort manual sería trabajo duplicado.
async fn listing(backend: &Backend, dir: &VPath) -> Result<(Vec<Entry>, Option<u64>), Error> {
    backend.list_with_skipped(dir).await
}

/// Primera página de `dir` (hasta [`FIRST_PAGE`]) más el stream con el RESTO
/// (o `None` si el dir cabía en la primera página) y las omitidas del
/// contenedor (#93). El primer render no espera al listado entero (ADR 0017).
/// Regla 7: el TUI no toca el FS.
async fn first_page(
    backend: &Backend,
    dir: &VPath,
) -> Result<(Vec<Entry>, Option<EntryStream>, Option<u64>), Error> {
    let (mut stream, skipped) = backend.list_stream(dir).await?;
    let mut first = Vec::with_capacity(FIRST_PAGE);
    while first.len() < FIRST_PAGE {
        match stream.next().await {
            Some(item) => first.push(item?),
            // El dir cabía en la primera página: no hay resto que drenar.
            None => return Ok((first, None, skipped)),
        }
    }
    Ok((first, Some(stream), skipped))
}

/// Arranca el drenador del RESTO del listado: envía lotes coalescidos al run
/// loop, que los aplica con [`Pane::extend_listing`]. Soltar el `rx` (un cd
/// nuevo) mata el drenador en su próximo envío → suelta el stream (regla 3).
fn spawn_fill(pane: usize, mut stream: EntryStream) -> Fill {
    // Bounded a 1: el drenador no corre por delante del run loop más de un
    // lote (backpressure); el pico de memoria es un lote, no todo el dir.
    let (tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(FILL_BATCH);
        let mut flush = tokio::time::interval(FILL_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        flush.tick().await; // consume el tick inmediato del interval
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(e)) => {
                        batch.push(e);
                        if batch.len() >= FILL_BATCH
                            && tx
                                .send(FillMsg::Batch(std::mem::take(&mut batch)))
                                .await
                                .is_err()
                        {
                            return; // el run loop soltó el rx (cd nuevo)
                        }
                    }
                    Some(Err(_)) => {
                        let _ = tx.send(FillMsg::Failed).await;
                        return;
                    }
                    None => {
                        if !batch.is_empty() {
                            let _ = tx.send(FillMsg::Batch(batch)).await;
                        }
                        return; // fin: drop(tx) cierra el canal → finish_listing
                    }
                },
                _ = flush.tick() => {
                    // Vacía un lote PARCIAL (progreso en streams lentos).
                    if !batch.is_empty()
                        && tx
                            .send(FillMsg::Batch(std::mem::take(&mut batch)))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    Fill { pane, rx }
}

/// cd CANCELABLE (regla 3): el listado corre contra el stream de eventos —
/// Esc lo abandona (el pane se queda donde estaba) y Ctrl-C sale del TUI
/// (atajos FIJOS durante un cd: aquí no aplica el keymap — son la salida de
/// emergencia y no deben ser remapeables a algo que no exista). Soltar el
/// future del listado detiene al productor del provider (testeado en
/// vfs-local). El resto de teclas se descartan mientras dura el cd.
async fn cd(app: &mut App, backend: &Backend, events: &mut EventStream, dir: VPath) -> Cd {
    // Historial (spec 2026-07-18): el dir ANTERIOR se captura AQUÍ y se
    // empuja solo en el brazo de ÉXITO (el pane se reemplazó de verdad).
    // Al vivir dentro de `cd` cubre TODOS los caminos que navegan —
    // nav.enter/nav.parent, quick-Enter (dispatch nav.enter), retry TOFU y
    // los popups de historial/hotlist — sin repetirlo por call-site.
    let prev = app.focused().dir().clone();
    let fut = first_page(backend, &dir);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            res = &mut fut => {
                match res {
                    Ok((first, stream, skipped)) => {
                        // #54: NO ordenamos aquí — `begin_listing` ->
                        // `PaneState::set_listing` normaliza internamente.
                        let more = stream.is_some();
                        app.focused_mut()
                            .begin_listing(dir.clone(), first, more, skipped);
                        let pane = app.focus();
                        // Un cd al MISMO dir (refresh-like) no ensucia el
                        // historial; el dedup consecutivo de `push` cubre
                        // el resto de redundancias.
                        if prev != dir {
                            app.history[pane].push(prev);
                        }
                        // Si queda stream, un drenador lo rellena en background.
                        return match stream {
                            Some(s) => Cd::Filling(spawn_fill(pane, s)),
                            None => Cd::Replaced(pane),
                        };
                    }
                    // Primer contacto TOFU (#45): en vez de una línea de
                    // error con la huella, abre el modal de confianza — `y`
                    // confía y REINTENTA esta misma navegación.
                    Err(Error::HostKeyUnknown {
                        host,
                        port,
                        algo,
                        fingerprint,
                    }) => {
                        app.modal = Some(Modal::TrustHostKey {
                            host,
                            port,
                            algo,
                            fingerprint,
                            dir: dir.clone(),
                        });
                        // El pane NO se tocó (solo se abrió el modal): Cancelled
                        // conserva un relleno en vuelo del listado anterior, que
                        // sigue siendo válido (MINOR del rust-reviewer).
                        return Cd::Cancelled;
                    }
                    // Un error de listado NO tumba el TUI: el pane se queda,
                    // pero un relleno previo de ESTE pane ya no aplica. El
                    // error se PORTA en el desenlace (popup de historial).
                    Err(e) => {
                        app.message = Some(error_message(&e));
                        return Cd::Failed(e);
                    }
                }
            }
            maybe = events.next() => {
                match maybe {
                    Some(Ok(Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                            app.quit = true;
                            return Cd::Cancelled;
                        }
                            (KeyCode::Esc, _) => return Cd::Cancelled,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return Cd::Cancelled,
                }
            }
        }
    }
}

#[cfg(test)]
mod archive_nav_tests {
    use super::*;
    use norte_proto::{Entry, EntryKind};

    fn entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("wire de test"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    #[test]
    fn archive_root_for_decide_por_extension_y_kind() {
        let e = entry("file:///d/A.ZIP", EntryKind::File);
        assert_eq!(
            archive_root_for(&e).expect("mayúsculas entran").to_wire(),
            "zip+file:///d/A.ZIP/!"
        );
        assert!(archive_root_for(&entry("file:///d/a.tar", EntryKind::File)).is_some());
        assert!(archive_root_for(&entry("file:///d/a.txt", EntryKind::File)).is_none());
        // Un dir llamado x.zip NO es contenedor; un symlink tampoco (v1).
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Dir)).is_none());
        assert!(archive_root_for(&entry("file:///d/x.zip", EntryKind::Symlink)).is_none());
        // #56 (antes v1 = no-op): Enter sobre un zip DENTRO de un tar
        // compone una capa más — anidamiento navegable.
        assert_eq!(
            archive_root_for(&entry("tar+file:///a.tar/!/i.zip", EntryKind::File))
                .expect("anidado navegable")
                .to_wire(),
            "zip+tar+file:///a.tar/!/i.zip/!"
        );
    }

    /// #55: `.tgz`/`.tar.gz` no coinciden con el token `tar+gz` vía el
    /// genérico `.{formato}` (el `+` no está en la extensión de archivo) —
    /// `EXT_ALIASES` los mapea explícitamente, case-insensitive, antes del
    /// genérico. `.tar`/`.zip` planos siguen funcionando sin pasar por el
    /// alias (`.tar.gz` NO debe casar `.tar`: termina en `.gz`).
    #[test]
    fn archive_root_for_extensiones_targz() {
        for wire in [
            "file:///d/a.tgz",
            "file:///d/a.tar.gz",
            "file:///d/A.TAR.GZ",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} debería ser navegable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        // Extensiones planas siguen funcionando (no capturadas por el alias).
        assert_eq!(
            archive_root_for(&entry("file:///d/a.tar", EntryKind::File))
                .expect("tar plano sigue")
                .scheme(),
            "tar+file"
        );
        assert_eq!(
            archive_root_for(&entry("file:///d/a.zip", EntryKind::File))
                .expect("zip plano sigue")
                .scheme(),
            "zip+file"
        );
    }

    /// Candado de encoding (#55, review): `ends_ci` es de BYTES y el compose
    /// no pasa por String — un nombre NO-UTF8 terminado en `.tgz` compone
    /// bien y sus bytes crudos sobreviven el wire (regla 1). Si alguien
    /// "simplifica" mañana con `to_str()`/lossy, esto se pone rojo.
    #[test]
    fn archive_root_for_targz_nombre_no_utf8() {
        for wire in [
            "file:///d/%FF%FE.tgz",
            "file:///d/a%F1o.TGZ",
            "file:///d/%FF.tar.gz",
        ] {
            let root = archive_root_for(&entry(wire, EntryKind::File))
                .unwrap_or_else(|| panic!("{wire} debería ser navegable"));
            assert_eq!(root.scheme(), "tar+gz+file", "wire={wire}");
        }
        assert_eq!(
            archive_root_for(&entry("file:///d/%FF%FE.tgz", EntryKind::File))
                .expect("no-UTF8 navegable")
                .to_wire(),
            "tar+gz+file:///d/%FF%FE.tgz/!",
            "los bytes crudos sobreviven el compose"
        );
    }
}

#[cfg(test)]
mod search_fill_tests {
    use super::{App, Fill, FillMsg, Pane, apply_fill_msg};
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire de test")
    }

    fn file(dir: &VPath, name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// review MAJOR T6: un dir grande PAGINÁNDOSE (fill vivo) + `Alt+F7` sobre
    /// ese pane → `begin_search` lo marca virtual y lo vacía; un lote POSTERIOR
    /// del drenador del listado REAL jamás debe entrar en el pane virtual (se
    /// colaría como hit — el propio root de la búsqueda entre los resultados).
    #[test]
    fn fill_no_contamina_el_pane_virtual() {
        let root = vp("file:///d");
        let mut app = App::new(
            Pane::new(root.clone(), vec![]),
            Pane::new(root.clone(), vec![]),
        );
        // Relleno paginado vivo del pane 0 (dir aún cargándose).
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        let mut fill = Some(Fill { pane: 0, rx });
        // Alt+F7 sobre el pane 0: pasa a virtual y se vacía.
        app.panes[0].begin_search(root.clone());
        // Llega un lote del drenador del listado REAL.
        apply_fill_msg(
            &mut app,
            &mut fill,
            0,
            Some(FillMsg::Batch(vec![
                file(&root, "real1"),
                file(&root, "real2"),
            ])),
        );
        assert!(
            app.panes[0].entries().is_empty(),
            "el listado real NO entra en el pane virtual"
        );
        assert!(fill.is_none(), "el fill obsoleto se suelta");
        assert!(
            app.panes[0].virtual_search,
            "el pane sigue en modo búsqueda"
        );
    }
}

#[cfg(test)]
mod apply_cd_tests {
    use super::{Cd, Fill, FillMsg, apply_cd};
    use norte_proto::Error;

    fn fill(pane: usize) -> Fill {
        let (_tx, rx) = tokio::sync::mpsc::channel::<FillMsg>(1);
        Fill { pane, rx }
    }

    /// Un REEMPLAZO del mismo pane suelta su relleno obsoleto.
    #[test]
    fn replaced_suelta_el_fill_del_pane() {
        let mut f = Some(fill(0));
        let mut lp = None;
        apply_cd(&mut f, &mut lp, Cd::Replaced(0));
        assert!(f.is_none(), "el fill del listado viejo se suelta");
    }

    /// Un reemplazo de OTRO pane no toca el relleno vivo.
    #[test]
    fn replaced_de_otro_pane_no_toca() {
        let mut f = Some(fill(0));
        let mut lp = None;
        apply_cd(&mut f, &mut lp, Cd::Replaced(1));
        assert!(f.is_some(), "el fill del pane 0 sobrevive");
    }

    /// #78: un cd FALLIDO NO suelta el relleno — el pane sigue en su listado
    /// anterior, que se sigue rellenando (soltarlo lo colgaba en loading).
    #[test]
    fn failed_conserva_el_fill() {
        let mut f = Some(fill(0));
        let mut lp = None;
        apply_cd(&mut f, &mut lp, Cd::Failed(Error::NotFound));
        assert!(
            f.is_some(),
            "el fill del listado anterior sigue vivo tras un cd fallido"
        );
    }

    /// Un cd abandonado no toca nada.
    #[test]
    fn cancelled_conserva_el_fill() {
        let mut f = Some(fill(0));
        let mut lp = None;
        apply_cd(&mut f, &mut lp, Cd::Cancelled);
        assert!(f.is_some());
    }
}
