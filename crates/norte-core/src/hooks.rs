//! Operation hooks (H1, ADR 0100): un plugin `hook` OBSERVA las entradas
//! que el journal ya registró, y lo único que puede devolver es una frase
//! para el humano.
//!
//! La fuente es el journal y no los handlers: [`crate::journal::Journal::
//! record_entry`] le ofrece cada fila comprometida a un [`HookSender`], así
//! que toda mutación —de cualquier frontend, de la CLI, de un agente, de un
//! lote, de un undo— llega por el mismo sitio (ADR 0077). El despachador
//! ([`spawn_dispatcher`]) vive FUERA del camino crítico: una cola acotada,
//! un drenado por tanda, un descubrimiento del registro por tanda (así una
//! aprobación recién dada vale en la siguiente) y una instancia por plugin
//! que se REUTILIZA entre tandas mientras su `.wasm` no cambie. Tres fallos
//! seguidos apagan los hooks de ese plugin hasta que se desactive y se
//! reactive, y se dice por el mismo canal que las frases.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use norte_plugin_host::{HOOK_EVENTS, HookInstance, PluginRuntime, hook_iface};
use norte_proto::methods::PluginNotice;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::plugins::{LocationMint, LocationSession, PluginRegistry, guest_reason};

/// Cuántos eventos caben en la cola entre el journal y el despachador. Por
/// encima, [`HookSender::offer`] descarta el MÁS NUEVO y lo cuenta: la
/// mutación ya ocurrió y no se va a frenar por un observador lento. Lo
/// descartado se le DICE al guest en la siguiente llamada (`dropped`).
pub const HOOK_QUEUE: usize = 1024;

/// Cuántos eventos se le entregan a un guest en una llamada como mucho. Es
/// también lo que acota el coste de un drenado: un lote de diez mil
/// renombrados llega en cuarenta llamadas, no en una ni en diez mil.
pub const HOOK_DRAIN_MAX: usize = 256;

/// Fallos SEGUIDOS —no instanció, atrapó, se pasó de presupuesto, rehusó—
/// tras los cuales los hooks de un plugin se apagan. Un éxito entre medias
/// pone el contador a cero; desactivar el plugin en el gestor lo rearma.
pub const HOOK_FUSE_FAILURES: u32 = 3;

/// Cuántos avisos puede soltar un plugin de golpe, y a qué ritmo se
/// repone el cupo: uno por segundo. Un hook es una frase por cosa que pasó,
/// no un canal; y sin tope una frase por tanda pisaría el único hueco de
/// mensaje transitorio de la barra — incluido el aviso de que sus propios
/// hooks se apagaron.
pub const HOOK_NOTICE_BURST: u32 = 4;

/// Una entrada del journal tal y como sale de `record_entry`: bytes de
/// cable, sin interpretar. Lo que el guest recibe se construye en el
/// despachador (`to_wire_events`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// El `seq` asignado.
    pub seq: i64,
    /// Milisegundos UTC del registro.
    pub ts_ms: i64,
    /// `"created" | "removed" | "trashed" | "renamed" | "mode_changed"`.
    pub op: String,
    /// `"user" | "agent" | "plugin"`. El id del actor NO viaja (ADR 0100).
    pub actor_kind: String,
    /// Path afectado (bytes `to_wire`).
    pub path: Vec<u8>,
    /// Destino de un `renamed` / modo nuevo de un `mode_changed`.
    pub path_to: Option<Vec<u8>>,
    /// Lote, si formó parte de uno.
    pub batch_id: Option<i64>,
}

/// El extremo del journal: ofrece eventos sin esperar nunca.
#[derive(Debug, Clone)]
pub struct HookSender {
    tx: mpsc::Sender<HookEvent>,
    /// Descartados desde el arranque (para mirar) y desde la última tanda
    /// (para decírselo al guest); el despachador vacía el segundo.
    dropped: Arc<AtomicU64>,
    dropped_since: Arc<AtomicU64>,
}

impl HookSender {
    /// Encola `ev` si cabe. Nunca bloquea ni falla: con la cola llena el
    /// evento se descarta y se cuenta ([`Self::dropped`]); con el despachador
    /// muerto, se descarta en silencio — no queda nadie a quien avisar.
    pub fn offer(&self, ev: HookEvent) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(ev) {
            self.dropped_since.fetch_add(1, Ordering::Relaxed);
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Una traza por potencia de dos: la primera dice que pasa, las
            // siguientes cuánto, y ninguna convierte una cola llena en un
            // registro lleno.
            if n.is_power_of_two() {
                tracing::warn!(dropped = n, "hooks: cola llena, eventos descartados");
            }
        }
    }

    /// Un par (extremo, receptor) sin despachador, para mirar lo que el
    /// journal ofrece.
    #[cfg(test)]
    pub(crate) fn for_test(capacity: usize) -> (Self, mpsc::Receiver<HookEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
                dropped_since: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// Cuántos eventos se descartaron por cola llena desde el arranque.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// A dónde van los avisos: el daemon los difunde a humanos por
/// `plugin.notice`; un frontend embebido los empuja a su canal.
pub trait HookNoticeSink: Send + Sync {
    /// Un aviso, ya enmascarado y acotado.
    fn notice(&self, n: PluginNotice);

    /// `true` cuando ya no hay nadie al otro lado: el despachador termina en
    /// la siguiente tanda. Es lo que ata la vida del despachador embebido a la
    /// del frontend que se llevó el canal.
    fn is_closed(&self) -> bool {
        false
    }
}

/// El fusible por plugin: cuenta fallos SEGUIDOS y apaga al llegar a
/// [`HOOK_FUSE_FAILURES`]. Puro, sin reloj, para poder probarlo.
#[derive(Debug, Default)]
pub(crate) struct Fuse {
    failures: HashMap<String, u32>,
    disabled: HashSet<String>,
}

impl Fuse {
    /// ¿Están apagados los hooks de `id`?
    pub(crate) fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    /// Una llamada que fue bien: el contador vuelve a cero.
    pub(crate) fn record_ok(&mut self, id: &str) {
        self.failures.remove(id);
    }

    /// Una llamada que falló. Devuelve `true` la vez que APAGA los hooks del
    /// plugin (y solo esa vez), para avisar una vez y no en cada tanda.
    pub(crate) fn record_failure(&mut self, id: &str) -> bool {
        if self.disabled.contains(id) {
            return false;
        }
        let n = self.failures.entry(id.to_owned()).or_insert(0);
        *n += 1;
        if *n >= HOOK_FUSE_FAILURES {
            self.disabled.insert(id.to_owned());
            self.failures.remove(id);
            return true;
        }
        false
    }

    /// Rearma los plugins que YA NO están consentidos: desactivar uno en el
    /// gestor (o retirarle la aprobación) es lo que el aviso de apagado le
    /// pide al lector, y tiene que ser verdad. El que vuelva a activarse
    /// empieza con el contador a cero.
    pub(crate) fn rearm_missing(&mut self, present: &HashSet<&str>) {
        self.disabled.retain(|id| present.contains(id.as_str()));
        self.failures.retain(|id, _| present.contains(id.as_str()));
    }
}

/// Un cupo de avisos por plugin: [`HOOK_NOTICE_BURST`] de golpe y uno por
/// segundo después. Puro sobre un instante que le pasan, para poder probarlo.
#[derive(Debug)]
pub(crate) struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(now: Instant) -> Self {
        Self {
            tokens: f64::from(HOOK_NOTICE_BURST),
            last: now,
        }
    }

    /// ¿Hay cupo para un aviso ahora? Consume uno si lo hay.
    pub(crate) fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed).min(f64::from(HOOK_NOTICE_BURST));
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Una instancia viva entre tandas, con lo que hace falta para saber si
/// sigue sirviendo: el `.wasm` que se instanció y su huella en disco.
struct Live {
    inst: HookInstance,
    wasm: PathBuf,
    stamp: Option<(std::time::SystemTime, u64)>,
}

/// Lo que el despachador conserva entre tandas. Bajo UN lock, y compartido
/// con la task bloqueante por `Arc`: si una tanda muere con un panic, lo que
/// había —los apagados, sobre todo— sigue ahí; un fallo del host no vuelve
/// a encender un plugin que se apagó por fallar.
#[derive(Default)]
struct State {
    fuse: Fuse,
    live: HashMap<String, Live>,
    /// Cuándo se vio por primera vez consentido cada plugin (ms UTC): un
    /// evento anterior a eso se registró antes de que el humano aprobara, y
    /// no se le entrega.
    first_seen: HashMap<String, i64>,
    buckets: HashMap<String, Bucket>,
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX)
}

/// Arranca el despachador y devuelve el extremo que se le instala al journal
/// ([`crate::Engine::enable_hooks`]) y la task, para quien quiera esperarla.
/// `config_dir` es donde viven `plugins/` y `plugins-state.toml`; el registro
/// se redescubre en cada tanda para que una aprobación recién dada cuente
/// sin reiniciar nada.
///
/// Termina con `cancel` (el apagado del daemon), cuando el `sink` dice que
/// ya no hay nadie al otro lado, o cuando muere el último [`HookSender`]. Una
/// llamada al guest en curso no se interrumpe —está acotada por el
/// presupuesto de época— pero no se espera: cancelar devuelve enseguida.
#[must_use]
pub fn spawn_dispatcher(
    config_dir: PathBuf,
    runtime: Arc<PluginRuntime>,
    sink: Arc<dyn HookNoticeSink>,
    cancel: CancellationToken,
) -> (HookSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<HookEvent>(HOOK_QUEUE);
    let sender = HookSender {
        tx,
        dropped: Arc::new(AtomicU64::new(0)),
        dropped_since: Arc::new(AtomicU64::new(0)),
    };
    let dropped_since = Arc::clone(&sender.dropped_since);
    let task = tokio::spawn(async move {
        let state = Arc::new(Mutex::new(State::default()));
        // Los plugins YA consentidos al arrancar reciben todo lo que llegue:
        // su aprobación es anterior a este proceso. Los que se aprueben
        // después empiezan en la tanda que primero los vea, y lo registrado
        // antes de esa tanda no se les entrega (podría ser anterior a la
        // aprobación, y no hay forma de saberlo).
        {
            let dir = config_dir.clone();
            let st = Arc::clone(&state);
            let seeded = tokio::task::spawn_blocking(move || {
                let ids = consented_hook_ids(&dir);
                let mut guard = st.lock().unwrap_or_else(PoisonError::into_inner);
                for id in ids {
                    guard.first_seen.insert(id, i64::MIN);
                }
            })
            .await;
            if let Err(e) = seeded {
                tracing::warn!(error = %e, "hooks: no se pudo leer el registro al arrancar");
            }
        }
        loop {
            let first = tokio::select! {
                () = cancel.cancelled() => break,
                got = rx.recv() => match got {
                    Some(ev) => ev,
                    None => break,
                },
            };
            if sink.is_closed() {
                break;
            }
            let mut batch = vec![first];
            while batch.len() < HOOK_DRAIN_MAX {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }
            // Dos `record_entry` concurrentes pueden ofrecer fuera de orden:
            // el `seq` se asigna bajo el lock de la cadena y la oferta va
            // después de soltarlo. El WIT promete orden de `seq`, y se cumple
            // aquí.
            batch.sort_unstable_by_key(|e| e.seq);
            let dropped = dropped_since.swap(0, Ordering::Relaxed);
            // Todo lo que sigue es I/O y CPU síncronos —descubrir el
            // registro, abrir directorios, correr wasm— así que va en
            // `spawn_blocking` (regla 2). El estado viaja por `Arc`: un panic
            // de la tanda no lo pierde.
            let dir = config_dir.clone();
            let rt = Arc::clone(&runtime);
            let st = Arc::clone(&state);
            let work = tokio::task::spawn_blocking(move || {
                let mut guard = st.lock().unwrap_or_else(PoisonError::into_inner);
                dispatch_batch(&dir, &rt, &mut guard, &batch, dropped)
            });
            let out = tokio::select! {
                () = cancel.cancelled() => break,
                out = work => out,
            };
            match out {
                Ok(notices) => {
                    for n in notices {
                        sink.notice(n);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "hooks: el despachador de una tanda murió");
                }
            }
        }
    });
    (sender, task)
}

/// El nombre del evento del manifiesto para una op del journal, o `None`
/// para una op que este binario no sabe nombrar (un journal más nuevo).
pub(crate) fn event_name_for(op: &str) -> Option<&'static str> {
    let wanted = format!("after-{}", op.replace('_', "-"));
    HOOK_EVENTS.iter().copied().find(|e| *e == wanted)
}

fn wire_op(op: &str) -> Option<hook_iface::Op> {
    Some(match op {
        "created" => hook_iface::Op::Created,
        "removed" => hook_iface::Op::Removed,
        "trashed" => hook_iface::Op::Trashed,
        "renamed" => hook_iface::Op::Renamed,
        "mode_changed" => hook_iface::Op::ModeChanged,
        _ => return None,
    })
}

fn wire_actor(kind: &str) -> Option<hook_iface::ActorKind> {
    Some(match kind {
        "user" => hook_iface::ActorKind::User,
        "agent" => hook_iface::ActorKind::Agent,
        "plugin" => hook_iface::ActorKind::Plugin,
        _ => return None,
    })
}

/// La forma de cable SIN userinfo: `sftp://ana@host/x` → `sftp://host/x`. Un
/// hook sin capacidad de red no tiene por qué aprender con qué usuario entra
/// el humano en cada máquina; el host y la ruta ya dicen qué cambió.
fn without_userinfo(wire: &str) -> String {
    let Some((scheme, rest)) = wire.split_once("://") else {
        return wire.to_owned();
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if rest.contains('/') {
        format!("{scheme}://{host}/{path}")
    } else {
        format!("{scheme}://{host}")
    }
}

/// Los eventos de una tanda que `ons` pide, en la forma del guest, sin los
/// que caen bajo una raíz protegida y sin los anteriores a `since_ms`. Con
/// `mint`, una sesión de ubicación por directorio PADRE distinto, que vive lo
/// que dure la llamada (se devuelven para que el llamante las sostenga); un
/// padre que es `$HOME` o la raíz del sistema no se abre — un hook mira el
/// resultado de una mutación, no el disco entero.
fn to_wire_events(
    batch: &[HookEvent],
    ons: &[String],
    since_ms: i64,
    protected: &[norte_proto::VPath],
    mint: Option<&Arc<LocationMint>>,
) -> (Vec<hook_iface::Event>, Vec<LocationSession>) {
    let mut sessions: Vec<LocationSession> = Vec::new();
    // `None` cacheado también: un padre que no se abre no se reintenta por
    // cada uno de sus doscientos hijos.
    let mut by_parent: BTreeMap<String, Option<usize>> = BTreeMap::new();
    let mut out = Vec::new();
    for ev in batch {
        if ev.ts_ms < since_ms {
            continue;
        }
        let Some(name) = event_name_for(&ev.op) else {
            continue;
        };
        if !ons.iter().any(|o| o == name) {
            continue;
        }
        let (Some(op), Some(actor)) = (wire_op(&ev.op), wire_actor(&ev.actor_kind)) else {
            continue;
        };
        // El path del journal es `VPath::to_wire`, o sea texto por
        // construcción; si no lo fuera, es una fila que este binario no
        // escribió y no se le pasa a nadie — y se dice.
        let Ok(path) = String::from_utf8(ev.path.clone()) else {
            tracing::warn!(seq = ev.seq, "hooks: fila con path no-UTF-8, saltada");
            continue;
        };
        let vpath = norte_proto::VPath::parse(&path).ok();
        if let Some(v) = &vpath
            && protected
                .iter()
                .any(|root| crate::policy::is_under(root, v))
        {
            // Bajo el estado del daemon no hay nada que un plugin deba ver,
            // ni siquiera el nombre.
            continue;
        }
        let path_to = ev
            .path_to
            .as_ref()
            .and_then(|b| String::from_utf8(b.clone()).ok())
            .map(|s| without_userinfo(&s));
        let leaf = vpath
            .as_ref()
            .and_then(|v| v.file_name().map(|s| s.as_bytes().to_vec()))
            .unwrap_or_default();
        let location = mint.and_then(|m| {
            let parent = vpath.as_ref()?.parent()?;
            if m.is_ceiling(&parent) {
                return None;
            }
            let key = parent.to_wire();
            let idx = if let Some(i) = by_parent.get(&key) {
                (*i)?
            } else {
                // Sin marcador y sin subir: el que corre es un plugin, y lo
                // que ve es el directorio de la mutación y nada más.
                let minted = m.mint_for(&parent, None, false).map(|s| {
                    sessions.push(s);
                    sessions.len() - 1
                });
                by_parent.insert(key, minted);
                minted?
            };
            let r = sessions[idx].as_ref();
            Some(hook_iface::LocationRef {
                token: r.token,
                prefix: r.prefix,
            })
        });
        out.push(hook_iface::Event {
            seq: u64::try_from(ev.seq).unwrap_or_default(),
            ts_ms: ev.ts_ms,
            op,
            actor,
            path: without_userinfo(&path),
            path_to,
            name: leaf,
            batch: ev.batch_id.and_then(|b| u64::try_from(b).ok()),
            location,
        });
    }
    (out, sessions)
}

/// Los ids de los hooks consentidos ahora mismo. BLOQUEANTE.
fn consented_hook_ids(config_dir: &std::path::Path) -> Vec<String> {
    PluginRegistry::discover(config_dir)
        .map(|reg| reg.resolve_hooks().into_iter().map(|(r, _)| r.0).collect())
        .unwrap_or_default()
}

fn stamp_of(wasm: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(wasm).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Una tanda contra todos los hooks consentidos. BLOQUEANTE.
#[tracing::instrument(skip_all, fields(batch_len = batch.len(), dropped))]
fn dispatch_batch(
    config_dir: &std::path::Path,
    runtime: &PluginRuntime,
    state: &mut State,
    batch: &[HookEvent],
    dropped: u64,
) -> Vec<PluginNotice> {
    let mut notices = Vec::new();
    let reg = match PluginRegistry::discover(config_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "hooks: no se pudo leer el registro de plugins");
            return notices;
        }
    };
    let hooks = reg.resolve_hooks();
    let present: HashSet<&str> = hooks.iter().map(|(r, _)| r.0.as_str()).collect();
    state.fuse.rearm_missing(&present);
    state.live.retain(|id, _| present.contains(id.as_str()));
    state
        .first_seen
        .retain(|id, _| present.contains(id.as_str()));
    let protected = crate::policy::protected_roots();
    let now = Instant::now();
    let now_ms = now_ms();
    for (resolved, ons) in hooks {
        let (id, _name, wasm, caps, settings) = resolved;
        if state.fuse.is_disabled(&id) {
            continue;
        }
        let since = *state.first_seen.entry(id.clone()).or_insert(now_ms);
        let mint = caps
            .location
            .granted()
            .then(|| LocationMint::new(norte_vfs_local::Bounds::default()));
        let (events, _sessions) = to_wire_events(batch, &ons, since, &protected, mint.as_ref());
        if events.is_empty() {
            continue;
        }
        let host: Option<Arc<dyn norte_plugin_host::LocationHost>> = mint
            .as_ref()
            .map(|m| Arc::clone(m) as Arc<dyn norte_plugin_host::LocationHost>);
        let outcome = ensure_live(state, runtime, &id, &wasm, caps).and_then(|live| {
            live.inst.set_location(host);
            live.inst.set_settings(settings);
            let out = live.inst.on_events(&events, dropped);
            // El token muere con `_sessions` al salir de la iteración; la
            // instancia se queda sin resolutor hasta la próxima tanda.
            live.inst.set_location(None);
            out.map_err(|e| e.to_string())
        });
        match outcome {
            Ok(Ok(effects)) => {
                state.fuse.record_ok(&id);
                notices.extend(speak(state, &id, effects, now));
            }
            Ok(Err(frase)) => {
                tracing::warn!(plugin = %id, reason = %guest_reason(&frase), "hook: el guest rehusó");
                state.live.remove(&id);
                if state.fuse.record_failure(&id) {
                    notices.push(disabled_notice(&id));
                }
            }
            Err(e) => {
                tracing::warn!(plugin = %id, error = %e, "hook: fallo al ejecutar");
                state.live.remove(&id);
                if state.fuse.record_failure(&id) {
                    notices.push(disabled_notice(&id));
                }
            }
        }
    }
    notices
}

/// La instancia viva de `id`, reutilizada mientras el `.wasm` sea el mismo
/// fichero sin cambiar: compilar un componente por tanda es lo que
/// convertiría «fuera del camino crítico» en «un hilo del pool ocupado todo
/// el lote». Instancia si hace falta; `Err` si no pudo.
fn ensure_live<'s>(
    state: &'s mut State,
    runtime: &PluginRuntime,
    id: &str,
    wasm: &std::path::Path,
    caps: norte_plugin_host::Capabilities,
) -> Result<&'s mut Live, String> {
    let stamp = stamp_of(wasm);
    let reuse = state
        .live
        .get(id)
        .is_some_and(|l| l.wasm == wasm && l.stamp == stamp && l.stamp.is_some());
    if !reuse {
        state.live.remove(id);
        let inst = runtime
            .instantiate_hook_with_location(wasm, caps, None)
            .map_err(|e| e.to_string())?;
        state.live.insert(
            id.to_owned(),
            Live {
                inst,
                wasm: wasm.to_path_buf(),
                stamp,
            },
        );
    }
    state
        .live
        .get_mut(id)
        .ok_or_else(|| "instancia perdida".to_owned())
}

/// Los efectos de una llamada convertidos en avisos: UNA frase por plugin y
/// tanda, y dentro del cupo del plugin; el resto se cuenta, no se pinta.
fn speak(
    state: &mut State,
    id: &str,
    effects: Vec<hook_iface::Effect>,
    now: Instant,
) -> Vec<PluginNotice> {
    let mut out = Vec::new();
    let mut dropped_effects = 0u32;
    for eff in effects {
        match eff {
            hook_iface::Effect::Notify(text) => {
                let bucket = state
                    .buckets
                    .entry(id.to_owned())
                    .or_insert_with(|| Bucket::new(now));
                if !out.is_empty() || !bucket.take(now) {
                    dropped_effects += 1;
                    continue;
                }
                out.push(PluginNotice {
                    plugin_id: id.to_owned(),
                    kind: KIND_NOTIFY.to_owned(),
                    text: Some(guest_reason(&text)),
                });
            }
        }
    }
    if dropped_effects > 0 {
        tracing::debug!(plugin = %id, dropped_effects, "hook: avisos fuera de cupo");
    }
    out
}

/// Las dos clases de aviso, las MISMAS cadenas que el proto declara en
/// `PLUGIN_NOTICE_KINDS`; un test lo ata.
const KIND_NOTIFY: &str = "notify";
const KIND_HOOKS_DISABLED: &str = "hooks-disabled";

fn disabled_notice(id: &str) -> PluginNotice {
    tracing::warn!(
        plugin = %id,
        failures = HOOK_FUSE_FAILURES,
        "hooks apagados: desactivar y reactivar el plugin los rearma"
    );
    PluginNotice {
        plugin_id: id.to_owned(),
        kind: KIND_HOOKS_DISABLED.to_owned(),
        text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, op: &str, path: &str) -> HookEvent {
        HookEvent {
            seq,
            ts_ms: 1_726_000_000_000,
            op: op.to_owned(),
            actor_kind: "user".to_owned(),
            path: path.as_bytes().to_vec(),
            path_to: None,
            batch_id: Some(7),
        }
    }

    #[test]
    fn las_clases_que_se_emiten_son_las_que_el_proto_declara() {
        use norte_proto::methods::PLUGIN_NOTICE_KINDS;
        assert!(PLUGIN_NOTICE_KINDS.contains(&KIND_NOTIFY));
        assert!(PLUGIN_NOTICE_KINDS.contains(&KIND_HOOKS_DISABLED));
        assert_eq!(
            PLUGIN_NOTICE_KINDS.len(),
            2,
            "una clase nueva llega con su emisor"
        );
    }

    #[test]
    fn el_fusible_apaga_al_tercer_fallo_seguido_y_avisa_una_vez() {
        let mut f = Fuse::default();
        assert!(!f.record_failure("a"));
        assert!(!f.record_failure("a"));
        f.record_ok("a");
        assert!(!f.record_failure("a"), "el éxito puso el contador a cero");
        assert!(!f.record_failure("a"));
        assert!(f.record_failure("a"), "el tercero seguido apaga");
        assert!(f.is_disabled("a"));
        assert!(!f.record_failure("a"), "apagado no vuelve a avisar");
        assert!(!f.is_disabled("b"), "cada plugin lleva su fusible");
        // Desactivar el plugin (deja de estar presente) rearma; al volver,
        // empieza de cero.
        f.rearm_missing(&HashSet::from(["b"]));
        assert!(!f.is_disabled("a"));
        assert!(!f.record_failure("a"), "contador a cero tras rearmar");
    }

    #[test]
    fn el_cupo_de_avisos_es_una_rafaga_y_uno_por_segundo() {
        let t0 = Instant::now();
        let mut b = Bucket::new(t0);
        for _ in 0..HOOK_NOTICE_BURST {
            assert!(b.take(t0));
        }
        assert!(!b.take(t0), "la ráfaga se agotó");
        assert!(
            b.take(t0 + std::time::Duration::from_secs(1)),
            "un segundo, uno más"
        );
        assert!(!b.take(t0 + std::time::Duration::from_millis(1100)));
    }

    #[test]
    fn el_nombre_del_evento_sale_de_la_op_del_journal() {
        assert_eq!(event_name_for("created"), Some("after-created"));
        assert_eq!(event_name_for("mode_changed"), Some("after-mode-changed"));
        assert_eq!(event_name_for("teleported"), None);
        for e in HOOK_EVENTS {
            assert!(e.starts_with("after-"), "{e}: solo hay after-*");
        }
    }

    #[test]
    fn el_userinfo_no_viaja_al_guest() {
        assert_eq!(without_userinfo("sftp://ana@host/a/b"), "sftp://host/a/b");
        assert_eq!(without_userinfo("sftp://ana@host"), "sftp://host");
        assert_eq!(without_userinfo("file:///a/b"), "file:///a/b");
        assert_eq!(without_userinfo("ftp://host/x"), "ftp://host/x");
    }

    #[test]
    fn los_eventos_se_filtran_por_lo_que_el_manifiesto_pide() {
        let batch = vec![
            ev(1, "created", "file:///a/b.txt"),
            ev(2, "renamed", "file:///a/c.txt"),
            ev(3, "vanished", "file:///a/d.txt"),
        ];
        let (out, sessions) = to_wire_events(&batch, &["after-renamed".to_owned()], 0, &[], None);
        assert!(sessions.is_empty(), "sin location no se acuña nada");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 2);
        assert_eq!(out[0].path, "file:///a/c.txt");
        assert_eq!(out[0].name, b"c.txt".to_vec());
        assert_eq!(out[0].batch, Some(7));
        assert!(out[0].location.is_none());
        assert!(matches!(out[0].op, hook_iface::Op::Renamed));
    }

    #[test]
    fn lo_anterior_a_la_aprobacion_y_lo_protegido_no_se_entrega() {
        let mut viejo = ev(1, "created", "file:///a/old.txt");
        viejo.ts_ms = 1;
        let batch = vec![
            viejo,
            ev(2, "created", "file:///cfg/norte/journal.db"),
            ev(3, "created", "file:///a/new.txt"),
        ];
        let protegida = norte_proto::VPath::parse("file:///cfg/norte").expect("vpath");
        let (out, _) = to_wire_events(
            &batch,
            &["after-created".to_owned()],
            1_000,
            std::slice::from_ref(&protegida),
            None,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 3);
    }

    #[test]
    fn una_sesion_de_ubicacion_por_directorio_padre_y_ninguna_en_el_techo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let root = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");
        let a = format!("{}/a.txt", root.to_wire());
        let b = format!("{}/b.txt", root.to_wire());
        let c = format!("{}/sub/c.txt", root.to_wire());
        let en_home = format!("{}/x.txt", root.to_wire());
        let batch = vec![
            ev(1, "created", &a),
            ev(2, "created", &b),
            ev(3, "created", &c),
            ev(4, "created", "file:///top.txt"),
        ];
        // Con la casa en `sub`: el padre de a/b se abre; el de c es la casa y
        // el de top.txt la raíz del sistema — techos los dos.
        let mint = LocationMint::with_protected_and_home(
            vec![],
            norte_vfs_local::Bounds::default(),
            Some(sub.clone()),
        );
        let (out, sessions) =
            to_wire_events(&batch, &["after-created".to_owned()], 0, &[], Some(&mint));
        assert_eq!(out.len(), 4, "el evento viaja aunque no haya token");
        assert_eq!(sessions.len(), 1, "un solo padre abierto");
        let t = |i: usize| out[i].location.as_ref().map(|l| l.token.clone());
        assert_eq!(t(0), t(1), "el mismo padre comparte token");
        assert!(t(0).is_some());
        assert!(t(2).is_none(), "la casa no se abre");
        assert!(t(3).is_none(), "la raíz del sistema tampoco");
        // Y con la casa en el directorio del test, a.txt tampoco.
        let mint = LocationMint::with_protected_and_home(
            vec![],
            norte_vfs_local::Bounds::default(),
            Some(dir.path().to_path_buf()),
        );
        let (out, sessions) = to_wire_events(
            &[ev(1, "created", &en_home)],
            &["after-created".to_owned()],
            0,
            &[],
            Some(&mint),
        );
        assert_eq!(out.len(), 1);
        assert!(sessions.is_empty());
    }

    struct Nadie;
    impl HookNoticeSink for Nadie {
        fn notice(&self, _n: PluginNotice) {}
    }

    /// Regla dura 3: el despachador es una task larga, y cancelar la termina
    /// aunque nunca llegue un evento.
    #[tokio::test]
    async fn cancelar_termina_el_despachador() {
        let cfg = tempfile::tempdir().expect("tempdir");
        let cancel = CancellationToken::new();
        let (tx, task) = spawn_dispatcher(
            cfg.path().to_path_buf(),
            Arc::new(PluginRuntime::new().expect("runtime")),
            Arc::new(Nadie),
            cancel.clone(),
        );
        tx.offer(ev(1, "created", "file:///a"));
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("termina al cancelar")
            .expect("sin panic");
        // El extremo sigue siendo inofensivo con el despachador muerto.
        tx.offer(ev(2, "created", "file:///b"));
    }

    /// Y un sink que ya no tiene a nadie detrás termina el despachador en la
    /// siguiente tanda: es la vida del embebido, atada a su frontend.
    #[tokio::test]
    async fn un_sink_cerrado_termina_el_despachador() {
        struct Cerrado;
        impl HookNoticeSink for Cerrado {
            fn notice(&self, _n: PluginNotice) {}
            fn is_closed(&self) -> bool {
                true
            }
        }
        let cfg = tempfile::tempdir().expect("tempdir");
        let (tx, task) = spawn_dispatcher(
            cfg.path().to_path_buf(),
            Arc::new(PluginRuntime::new().expect("runtime")),
            Arc::new(Cerrado),
            CancellationToken::new(),
        );
        tx.offer(ev(1, "created", "file:///a"));
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("termina al ver el sink cerrado")
            .expect("sin panic");
    }
}
