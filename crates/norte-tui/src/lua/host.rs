//! Cargar el host Lua, resolver su confianza y correr sus comandos.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! el bucle de eventos era el único sitio desde el que se podía hablar con el
//! host, y sus tests tenían que vivir dentro de `main.rs`.
//!
//! El resto de `lua/` es el host en sí (`api`), el driver de comandos
//! (`driver`), la API de fs (`fs`), la barra (`statusbar`) y la confianza
//! (`trust`). Esto es el pegamento de arriba: qué se carga, en qué orden, y qué
//! se le dice al lector cuando algo de eso falla.

use std::collections::VecDeque;

use tokio_util::sync::CancellationToken;

use crossterm::event::KeyCode;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use sha2::Digest as _;

use super::{CommandRun, Layer, LuaHost, PaneCtx, StatusInput, TrustDecision, TrustStore};
use crate::app::{App, DialogOutcome, Modal, detail_for_bar, io_error_category, trust_lua_key};
use crate::config::Layers;

/// Tope de la cola FIFO de comandos Lua (M4): con un run en vuelo, los
/// siguientes se encolan hasta aquí; llena, solo queda el aviso.
const LUA_QUEUE_MAX: usize = 8;

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
/// TOFU (`load_lua_project`, privada). La carga NO toca el `Backend` (solo evalúa
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
pub async fn load_lua(app: &mut App, layers: &Layers) -> Option<LuaHost> {
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
    let Some(state) = norte_config::dirs::state_dir() else {
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
pub async fn resolve_lua_trust(app: &mut App, host: Option<&LuaHost>, code: KeyCode) {
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
            let dir = norte_config::dirs::state_dir().ok_or_else(|| {
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
pub fn start_lua_run(
    app: &mut App,
    host: &LuaHost,
    backend: &Backend,
    name: &str,
) -> Option<(CommandRun, CancellationToken)> {
    let pane = app.focused();
    let other = &app.panes[app.target_index().unwrap_or_else(|| app.focus())];
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
/// tope `LUA_QUEUE_MAX`; llena = solo el aviso); libre, arranca. Sin host
/// (mlua no arrancó) el comando no puede existir → `err-lua-unknown`.
pub fn run_lua_command(
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
pub fn refresh_lua_status(app: &mut App, lua_host: Option<&LuaHost>) {
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
